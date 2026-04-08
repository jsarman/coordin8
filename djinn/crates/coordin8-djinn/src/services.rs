//! Djinn service boot functions.
//!
//! Each function binds a gRPC server and blocks until the listener closes.
//! Use `tokio::spawn` in tests to run multiple services concurrently.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::broadcast;
use tonic::transport::Server;
use tracing::info;

use coordin8_core::{EventStore, LeaseStore, RegistryStore, SpaceStore, TxnStore};
use coordin8_event::{EventManager, EventServiceImpl};
use coordin8_lease::{LeaseManager, LeaseServiceImpl};
use coordin8_proto::coordin8::event_service_server::EventServiceServer;
use coordin8_proto::coordin8::lease_service_server::LeaseServiceServer;
use coordin8_proto::coordin8::participant_service_server::ParticipantServiceServer;
use coordin8_proto::coordin8::proxy_service_server::ProxyServiceServer;
use coordin8_proto::coordin8::registry_service_server::RegistryServiceServer;
use coordin8_proto::coordin8::space_service_server::SpaceServiceServer;
use coordin8_proto::coordin8::transaction_service_server::TransactionServiceServer;
use coordin8_provider_local::{
    InMemoryEventStore, InMemoryLeaseStore, InMemoryRegistryStore, InMemorySpaceStore,
    InMemoryTxnStore,
};
use coordin8_proxy::{ProxyConfig, ProxyManager, ProxyServiceImpl};
use coordin8_registry::service::RegistryBroadcast;
use coordin8_registry::{store::RegistryIndex, RegistryServiceImpl};
use coordin8_space::{SpaceManager, SpaceParticipantService, SpaceServiceImpl};
use coordin8_txn::{TxnManager, TxnServiceImpl};

// ── Env var helpers (pub for tests) ──────────────────────────────────────────

/// Read `COORDIN8_BIND_ADDR`, defaulting to `0.0.0.0:0` (OS-assigned port).
pub fn bind_addr() -> String {
    std::env::var("COORDIN8_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:0".to_string())
}

/// Read `COORDIN8_ADVERTISE_HOST`, defaulting to `127.0.0.1`.
///
/// The port registered with Registry always comes from the bound listener.
/// Override this in Docker so peers dial the container name. Mirrors the
/// `ADVERTISE_HOST` convention used by the Go example services.
pub fn advertise_host() -> String {
    std::env::var("COORDIN8_ADVERTISE_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}

// ── Monolith boot ─────────────────────────────────────────────────────────────

/// Boot every service in a single process on fixed ports (the original monolith).
///
/// This is `djinn` / `djinn all`. Zero behavioral changes from the original
/// `main.rs`.
pub async fn run_all() -> Result<()> {
    info!("Djinn starting...");

    // ── Layer 0: Provider ────────────────────────────────────────────────────
    let provider = std::env::var("COORDIN8_PROVIDER").unwrap_or_else(|_| "local".into());

    type Stores = (
        Arc<dyn LeaseStore>,
        Arc<dyn RegistryStore>,
        Arc<dyn EventStore>,
        Arc<dyn TxnStore>,
        Arc<dyn SpaceStore>,
    );
    let (lease_store, registry_store, event_store, txn_store, space_store): Stores =
        match provider.as_str() {
            "dynamo" => {
                let client = coordin8_provider_dynamo::make_dynamo_client().await;

                let lease_store = Arc::new(coordin8_provider_dynamo::DynamoLeaseStore::new(
                    client.clone(),
                ));
                lease_store.init().await?;

                let registry_store = Arc::new(coordin8_provider_dynamo::DynamoRegistryStore::new(
                    client.clone(),
                ));
                registry_store.init().await?;

                let event_store = Arc::new(coordin8_provider_dynamo::DynamoEventStore::new(
                    client.clone(),
                ));
                event_store.init().await?;

                let txn_store = Arc::new(coordin8_provider_dynamo::DynamoTxnStore::new(
                    client.clone(),
                ));
                txn_store.init().await?;

                let space_store = Arc::new(coordin8_provider_dynamo::DynamoSpaceStore::new(
                    client.clone(),
                ));
                space_store.init().await?;

                info!("  ✓ Provider: dynamo (DynamoDB)");

                (
                    lease_store,
                    registry_store,
                    event_store,
                    txn_store,
                    space_store,
                )
            }
            _ => {
                let lease_store: Arc<dyn LeaseStore> = Arc::new(InMemoryLeaseStore::new());
                let registry_store: Arc<dyn RegistryStore> = Arc::new(InMemoryRegistryStore::new());
                let event_store: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
                let txn_store: Arc<dyn TxnStore> = Arc::new(InMemoryTxnStore::new());
                let space_store: Arc<dyn SpaceStore> = Arc::new(InMemorySpaceStore::new());
                info!("  ✓ Provider: local (in-memory)");
                (
                    lease_store,
                    registry_store,
                    event_store,
                    txn_store,
                    space_store,
                )
            }
        };

    // ── Layer 1: LeaseMgr ────────────────────────────────────────────────────
    let (expiry_tx, _) = broadcast::channel::<coordin8_core::LeaseRecord>(256);
    let lease_config = coordin8_core::LeaseConfig::from_env();
    info!(
        "  Lease policy: max_ttl={}, preferred_ttl={}s",
        lease_config
            .max_ttl
            .map_or("FOREVER".to_string(), |v| format!("{}s", v)),
        lease_config.preferred_ttl
    );
    let lease_manager = Arc::new(LeaseManager::new(lease_store, lease_config));

    let reaper_manager = Arc::clone(&lease_manager);
    let reaper_tx = expiry_tx.clone();
    tokio::spawn(async move {
        coordin8_lease::reaper::run_reaper(reaper_manager, reaper_tx, Duration::from_secs(1)).await;
    });
    info!("  ✓ LeaseMgr: ready");

    // ── Layer 2a: Registry ───────────────────────────────────────────────────
    let (registry_tx, _): (RegistryBroadcast, _) = broadcast::channel(256);
    let registry_index = Arc::new(RegistryIndex::new(registry_store.clone()));

    let registry_expiry_index = Arc::clone(&registry_index);
    let registry_expiry_tx = registry_tx.clone();
    let mut registry_expiry_rx = expiry_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(lease) = registry_expiry_rx.recv().await {
            if lease.resource_id.starts_with("registry:") {
                if let Ok(Some(entry)) = registry_expiry_index
                    .unregister_by_lease(&lease.lease_id)
                    .await
                {
                    tracing::debug!(
                        capability_id = %entry.capability_id,
                        interface = %entry.interface,
                        lease_id = %lease.lease_id,
                        "registry entry expired"
                    );
                    let _ =
                        registry_expiry_tx.send(coordin8_registry::service::RegistryChangedEvent {
                            event_type: 1,
                            entry,
                        });
                }
            }
        }
    });
    info!("  ✓ Registry: ready");

    // ── Layer 2b: EventMgr ───────────────────────────────────────────────────
    let (event_tx, _) = broadcast::channel::<coordin8_core::EventRecord>(256);
    let event_manager = Arc::new(EventManager::new(
        event_store,
        Arc::clone(&lease_manager),
        event_tx,
    ));

    let event_expiry_mgr = Arc::clone(&event_manager);
    let mut event_expiry_rx = expiry_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(lease) = event_expiry_rx.recv().await {
            if lease.resource_id.starts_with("event:") {
                let _ = event_expiry_mgr.unsubscribe_by_lease(&lease.lease_id).await;
            }
        }
    });
    info!("  ✓ EventMgr: ready");

    // ── Layer 3: Proxy ───────────────────────────────────────────────────────
    let proxy_config = ProxyConfig::from_env();
    let proxy_manager = Arc::new(ProxyManager::new(registry_store, proxy_config));
    info!("  ✓ Proxy: ready");

    // ── Layer 4: TransactionMgr ───────────────────────────────────────────────
    let txn_manager = Arc::new(TxnManager::new(txn_store, Arc::clone(&lease_manager)));

    let txn_expiry_mgr = Arc::clone(&txn_manager);
    let mut txn_expiry_rx = expiry_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(lease) = txn_expiry_rx.recv().await {
            if let Some(txn_id) = lease.resource_id.strip_prefix("txn:") {
                let _ = txn_expiry_mgr.abort_expired(txn_id).await;
            }
        }
    });
    info!("  ✓ TransactionMgr: ready");

    // ── Layer 2c: Space ──────────────────────────────────────────────────────
    let (space_tuple_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    let (space_expiry_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    let space_manager = Arc::new(SpaceManager::new(
        space_store,
        Arc::clone(&lease_manager),
        space_tuple_tx,
        space_expiry_tx,
    ));

    let space_expiry_mgr = Arc::clone(&space_manager);
    let mut space_expiry_rx = expiry_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(lease) = space_expiry_rx.recv().await {
            if lease.resource_id.starts_with("space:") {
                space_expiry_mgr.on_tuple_expired(&lease.lease_id).await;
            } else if lease.resource_id.starts_with("space-watch:") {
                space_expiry_mgr.on_watch_expired(&lease.lease_id).await;
            }
        }
    });
    info!("  ✓ Space: ready");

    // ── gRPC servers ─────────────────────────────────────────────────────────
    let lease_addr = "0.0.0.0:9001".parse()?;
    let registry_addr = "0.0.0.0:9002".parse()?;
    let proxy_addr = "0.0.0.0:9003".parse()?;
    let txn_addr = "0.0.0.0:9004".parse()?;
    let event_addr = "0.0.0.0:9005".parse()?;
    let space_addr = "0.0.0.0:9006".parse()?;

    let lease_svc =
        LeaseServiceServer::new(LeaseServiceImpl::new(Arc::clone(&lease_manager), expiry_tx));
    let registry_svc = RegistryServiceServer::new(RegistryServiceImpl::new(
        registry_index,
        Arc::clone(&lease_manager),
        registry_tx,
    ));
    let proxy_svc = ProxyServiceServer::new(ProxyServiceImpl::new(proxy_manager));
    let txn_svc = TransactionServiceServer::new(TxnServiceImpl::new(txn_manager));
    let event_svc = EventServiceServer::new(EventServiceImpl::new(event_manager));
    let space_svc = SpaceServiceServer::new(SpaceServiceImpl::new(Arc::clone(&space_manager)));
    let space_participant_svc =
        ParticipantServiceServer::new(SpaceParticipantService::new(space_manager));

    info!("  ✓ LeaseMgr:      listening on {}", lease_addr);
    info!("  ✓ Registry:      listening on {}", registry_addr);
    info!("  ✓ Proxy:         listening on {}", proxy_addr);
    info!("  ✓ TransactionMgr: listening on {}", txn_addr);
    info!("  ✓ EventMgr:      listening on {}", event_addr);
    info!("  ✓ Space:         listening on {}", space_addr);
    info!("Djinn ready.");

    tokio::try_join!(
        Server::builder().add_service(lease_svc).serve(lease_addr),
        Server::builder()
            .add_service(registry_svc)
            .serve(registry_addr),
        Server::builder().add_service(proxy_svc).serve(proxy_addr),
        Server::builder().add_service(txn_svc).serve(txn_addr),
        Server::builder().add_service(event_svc).serve(event_addr),
        Server::builder()
            .add_service(space_svc)
            .add_service(space_participant_svc)
            .serve(space_addr),
    )?;

    Ok(())
}

// ── Registry alone ────────────────────────────────────────────────────────────

/// Boot Registry alone on the given bind address.
///
/// Registry is the well-known anchor and does not self-register. Includes its
/// own local LeaseMgr for the `registry:` lease namespace.
///
/// Pass `bind` as `"0.0.0.0:0"` to let the OS assign a port. Use
/// `serve_with_incoming` if you need the actual port before serving — see
/// [`run_registry_on_listener`] for the test-friendly version.
pub async fn run_registry() -> Result<()> {
    let bind = bind_addr();
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    let actual_addr = listener.local_addr()?;
    info!("Djinn registry starting on {actual_addr}...");
    run_registry_on_listener(listener).await
}

/// Boot Registry on a pre-bound [`TcpListener`].
///
/// This variant is test-friendly: bind port 0, read the actual address, then
/// pass the listener here. The caller knows the exact address before the server
/// starts accepting.
pub async fn run_registry_on_listener(listener: tokio::net::TcpListener) -> Result<()> {
    let registry_store: Arc<dyn RegistryStore> = Arc::new(InMemoryRegistryStore::new());

    let lease_store: Arc<dyn LeaseStore> = Arc::new(InMemoryLeaseStore::new());
    let lease_config = coordin8_core::LeaseConfig::from_env();
    let lease_manager = Arc::new(LeaseManager::new(lease_store, lease_config));

    let (expiry_tx, _) = broadcast::channel::<coordin8_core::LeaseRecord>(256);
    let reaper_manager = Arc::clone(&lease_manager);
    let reaper_tx = expiry_tx.clone();
    tokio::spawn(async move {
        coordin8_lease::reaper::run_reaper(reaper_manager, reaper_tx, Duration::from_secs(1)).await;
    });

    let (registry_tx, _): (RegistryBroadcast, _) = broadcast::channel(256);
    let registry_index = Arc::new(RegistryIndex::new(registry_store));

    let registry_expiry_index = Arc::clone(&registry_index);
    let registry_expiry_tx = registry_tx.clone();
    let mut registry_expiry_rx = expiry_tx.subscribe();
    tokio::spawn(async move {
        while let Ok(lease) = registry_expiry_rx.recv().await {
            if lease.resource_id.starts_with("registry:") {
                if let Ok(Some(entry)) = registry_expiry_index
                    .unregister_by_lease(&lease.lease_id)
                    .await
                {
                    tracing::debug!(
                        capability_id = %entry.capability_id,
                        "registry entry expired (split mode)"
                    );
                    let _ =
                        registry_expiry_tx.send(coordin8_registry::service::RegistryChangedEvent {
                            event_type: 1,
                            entry,
                        });
                }
            }
        }
    });

    let registry_svc = RegistryServiceServer::new(RegistryServiceImpl::new(
        registry_index,
        Arc::clone(&lease_manager),
        registry_tx,
    ));

    info!(
        "  ✓ Registry (split): listening on {}",
        listener.local_addr()?
    );

    Server::builder()
        .add_service(registry_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
        .await?;

    Ok(())
}

// ── LeaseMgr alone ───────────────────────────────────────────────────────────

/// Boot LeaseMgr alone. Reads `COORDIN8_BIND_ADDR` and `COORDIN8_REGISTRY`.
///
/// If `COORDIN8_REGISTRY` is set, self-registers under `interface=LeaseMgr`
/// with a 30-second self-lease. Otherwise warns and runs standalone.
pub async fn run_lease() -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&bind_addr()).await?;
    let host = advertise_host();
    let registry_addr = std::env::var("COORDIN8_REGISTRY").ok();
    run_lease_on_listener(listener, registry_addr.as_deref(), &host, 30).await
}

/// Boot LeaseMgr on a pre-bound [`TcpListener`].
///
/// `registry_addr` — if `Some`, self-registers with this Registry endpoint.
/// `advertise_host` — host peers should use to dial this LeaseMgr. The port
/// is taken from the listener (so `bind_addr = 0.0.0.0:0` still registers the
/// OS-assigned port correctly).
/// `self_lease_ttl` — TTL in seconds for the self-registration lease.
pub async fn run_lease_on_listener(
    listener: tokio::net::TcpListener,
    registry_addr: Option<&str>,
    advertise_host: &str,
    self_lease_ttl: u64,
) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let advertise_port = actual_addr.port();

    let lease_store: Arc<dyn LeaseStore> = Arc::new(InMemoryLeaseStore::new());
    let lease_config = coordin8_core::LeaseConfig::from_env();
    let lease_manager = Arc::new(LeaseManager::new(lease_store, lease_config));

    let (expiry_tx, _) = broadcast::channel::<coordin8_core::LeaseRecord>(256);
    let reaper_manager = Arc::clone(&lease_manager);
    let reaper_tx = expiry_tx.clone();
    tokio::spawn(async move {
        coordin8_lease::reaper::run_reaper(reaper_manager, reaper_tx, Duration::from_secs(1)).await;
    });

    let lease_svc =
        LeaseServiceServer::new(LeaseServiceImpl::new(Arc::clone(&lease_manager), expiry_tx));

    info!(
        "  ✓ LeaseMgr (split): listening on {actual_addr}, advertising {advertise_host}:{advertise_port}"
    );

    let registry_url = registry_addr.map(str::to_string);
    let advertise_host_owned = advertise_host.to_string();

    // Self-registration runs concurrently with the server via select!. The
    // handle is held as a local in `register_fut`, so when the server exits
    // OR the outer task is aborted, the future drops → handle drops → the
    // renewal task stops and the Registry entry expires on its own TTL.
    let register_fut = async move {
        let Some(registry_url) = registry_url else {
            tracing::warn!(
                "COORDIN8_REGISTRY not set — LeaseMgr running standalone (no self-registration)"
            );
            std::future::pending::<()>().await;
            return;
        };

        let registry_client = loop {
            match coordin8_proto::coordin8::registry_service_client::RegistryServiceClient::connect(
                registry_url.clone(),
            )
            .await
            {
                Ok(c) => break c,
                Err(e) => {
                    tracing::warn!("registry dial failed: {e}, retrying in 500ms");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        };

        match coordin8_bootstrap::self_register(
            registry_client,
            "LeaseMgr",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await
        {
            Ok(_handle) => {
                info!(
                    "  ✓ LeaseMgr: self-registered (capability: {}, lease: {})",
                    _handle.capability_id(),
                    _handle.lease_id()
                );
                // Hold the handle for the lifetime of this future so the
                // renewal task stays alive only while we are.
                std::future::pending::<()>().await;
            }
            Err(e) => {
                tracing::error!("self_register failed: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    let server_fut = Server::builder()
        .add_service(lease_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));

    info!("Djinn lease ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}
