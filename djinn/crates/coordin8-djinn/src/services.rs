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

use coordin8_bootstrap::{
    self_register, watch_expiry_prefix, RemoteCapabilityResolver, RemoteLeasing,
};
use coordin8_core::{EventStore, LeaseStore, Leasing, RegistryStore, SpaceStore, TxnStore};
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
use coordin8_proxy::{LocalCapabilityResolver, ProxyConfig, ProxyManager, ProxyServiceImpl};
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
    let leasing: Arc<dyn Leasing> = lease_manager.clone();

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
        Arc::clone(&leasing),
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
    let proxy_resolver = Arc::new(LocalCapabilityResolver::new(registry_store));
    let proxy_manager = Arc::new(ProxyManager::new(proxy_resolver, proxy_config));
    info!("  ✓ Proxy: ready");

    // ── Layer 4: TransactionMgr ───────────────────────────────────────────────
    let txn_manager = Arc::new(TxnManager::new(txn_store, Arc::clone(&leasing)));

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
        Arc::clone(&leasing),
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
        Arc::clone(&leasing),
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
    let leasing: Arc<dyn Leasing> = lease_manager.clone();

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
        Arc::clone(&leasing),
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
    run_lease_on_listener_with_shutdown(
        listener,
        registry_addr,
        advertise_host,
        self_lease_ttl,
        std::future::pending::<()>(),
    )
    .await
}

/// Same as [`run_lease_on_listener`] but exits cleanly when `shutdown`
/// resolves. Chaos tests use this to initiate a real graceful shutdown that
/// actually tears down in-flight HTTP/2 connections — a plain
/// `JoinHandle::abort()` only cancels the outer task and leaves tonic's
/// detached per-connection workers serving their existing streams.
pub async fn run_lease_on_listener_with_shutdown(
    listener: tokio::net::TcpListener,
    registry_addr: Option<&str>,
    advertise_host: &str,
    self_lease_ttl: u64,
    shutdown: impl std::future::Future<Output = ()>,
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

        match self_register(
            registry_client,
            "LeaseMgr",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await
        {
            Ok(handle) => {
                info!(
                    "  ✓ LeaseMgr: self-registered (capability: {}, lease: {})",
                    handle.capability_id(),
                    handle.lease_id()
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
        .serve_with_incoming_shutdown(
            tokio_stream::wrappers::TcpListenerStream::new(listener),
            shutdown,
        );

    info!("Djinn lease ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}

// ── EventMgr alone ───────────────────────────────────────────────────────────

/// Boot EventMgr alone. Reads `COORDIN8_BIND_ADDR` and `COORDIN8_REGISTRY`.
///
/// Requires `COORDIN8_REGISTRY` to be set — EventMgr needs a Registry to
/// discover LeaseMgr through and to self-register into.
pub async fn run_event() -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&bind_addr()).await?;
    let host = advertise_host();
    let registry_addr = std::env::var("COORDIN8_REGISTRY")
        .map_err(|_| anyhow::anyhow!("COORDIN8_REGISTRY must be set for split-mode EventMgr"))?;
    run_event_on_listener(listener, &registry_addr, &host, 30).await
}

/// Boot EventMgr on a pre-bound [`TcpListener`], wiring it to a remote
/// LeaseMgr discovered through Registry.
///
/// - `registry_addr` — Registry endpoint used for LeaseMgr discovery and
///   self-registration under `interface=EventMgr`.
/// - `advertise_host` — host peers should use to dial this EventMgr.
/// - `self_lease_ttl` — TTL in seconds for the self-registration lease.
///
/// Uses [`RemoteLeasing`] for grant/renew/cancel and
/// [`watch_expiry_prefix`] to drive subscription cleanup when leases expire
/// on the remote LeaseMgr.
pub async fn run_event_on_listener(
    listener: tokio::net::TcpListener,
    registry_addr: &str,
    advertise_host: &str,
    self_lease_ttl: u64,
) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let advertise_port = actual_addr.port();

    // Discover LeaseMgr through Registry. This blocks until Registry and
    // LeaseMgr are both reachable.
    let remote_leasing = RemoteLeasing::connect(registry_addr).await?;
    let leasing: Arc<dyn Leasing> = Arc::new(remote_leasing);

    // Local in-memory event store — events live in the EventMgr process.
    let event_store: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let (event_tx, _) = broadcast::channel::<coordin8_core::EventRecord>(256);
    let event_manager = Arc::new(EventManager::new(
        event_store,
        Arc::clone(&leasing),
        event_tx,
    ));

    // Drive subscription cleanup off remote LeaseMgr expiry events. Needs
    // its own LeaseServiceClient for the streaming WatchExpiry RPC — the
    // stream reconnects itself via Registry on failure.
    let lease_stream_client = coordin8_bootstrap::discover_lease_mgr(registry_addr).await?;
    let expiry_event_manager = Arc::clone(&event_manager);
    let expiry_registry = registry_addr.to_string();
    tokio::spawn(async move {
        watch_expiry_prefix(
            lease_stream_client,
            expiry_registry,
            "event:".to_string(),
            move |evt| {
                let mgr = Arc::clone(&expiry_event_manager);
                let lease_id = evt.lease_id;
                tokio::spawn(async move {
                    let _ = mgr.unsubscribe_by_lease(&lease_id).await;
                });
            },
        )
        .await;
    });

    let event_svc = EventServiceServer::new(EventServiceImpl::new(Arc::clone(&event_manager)));

    info!(
        "  ✓ EventMgr (split): listening on {actual_addr}, advertising {advertise_host}:{advertise_port}"
    );

    let registry_url = registry_addr.to_string();
    let advertise_host_owned = advertise_host.to_string();

    let register_fut = async move {
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

        match self_register(
            registry_client,
            "EventMgr",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await
        {
            Ok(handle) => {
                info!(
                    "  ✓ EventMgr: self-registered (capability: {}, lease: {})",
                    handle.capability_id(),
                    handle.lease_id()
                );
                std::future::pending::<()>().await;
            }
            Err(e) => {
                tracing::error!("self_register failed: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    let server_fut = Server::builder()
        .add_service(event_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));

    info!("Djinn event ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}

// ── Space alone ──────────────────────────────────────────────────────────────

/// Boot Space alone. Reads `COORDIN8_BIND_ADDR` and `COORDIN8_REGISTRY`.
///
/// Requires `COORDIN8_REGISTRY` — Space needs a Registry to discover
/// LeaseMgr through and to self-register into.
pub async fn run_space() -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&bind_addr()).await?;
    let host = advertise_host();
    let registry_addr = std::env::var("COORDIN8_REGISTRY")
        .map_err(|_| anyhow::anyhow!("COORDIN8_REGISTRY must be set for split-mode Space"))?;
    run_space_on_listener(listener, &registry_addr, &host, 30).await
}

/// Boot Space on a pre-bound [`TcpListener`], wired to a remote LeaseMgr.
///
/// Mounts both `SpaceServiceImpl` and `SpaceParticipantService` on the same
/// server, matching the monolith layout. Uses `watch_expiry_prefix` twice —
/// once for `space:` (tuple leases) and once for `space-watch:` (watch
/// leases) — to keep tuple and watch cleanup driven by the remote LeaseMgr.
pub async fn run_space_on_listener(
    listener: tokio::net::TcpListener,
    registry_addr: &str,
    advertise_host: &str,
    self_lease_ttl: u64,
) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let advertise_port = actual_addr.port();

    let remote_leasing = RemoteLeasing::connect(registry_addr).await?;
    let leasing: Arc<dyn Leasing> = Arc::new(remote_leasing);

    let space_store: Arc<dyn SpaceStore> = Arc::new(InMemorySpaceStore::new());
    let (space_tuple_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    let (space_expiry_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    let space_manager = Arc::new(SpaceManager::new(
        space_store,
        Arc::clone(&leasing),
        space_tuple_tx,
        space_expiry_tx,
    ));

    // Remote LeaseMgr drives tuple + watch cleanup via two WatchExpiry
    // streams. Each needs its own LeaseServiceClient because the underlying
    // gRPC stream is owned by one consumer.
    let tuple_stream_client = coordin8_bootstrap::discover_lease_mgr(registry_addr).await?;
    let tuple_mgr = Arc::clone(&space_manager);
    let tuple_registry = registry_addr.to_string();
    tokio::spawn(async move {
        watch_expiry_prefix(
            tuple_stream_client,
            tuple_registry,
            "space:".to_string(),
            move |evt| {
                let mgr = Arc::clone(&tuple_mgr);
                let lease_id = evt.lease_id;
                tokio::spawn(async move {
                    mgr.on_tuple_expired(&lease_id).await;
                });
            },
        )
        .await;
    });

    let watch_stream_client = coordin8_bootstrap::discover_lease_mgr(registry_addr).await?;
    let watch_mgr = Arc::clone(&space_manager);
    let watch_registry = registry_addr.to_string();
    tokio::spawn(async move {
        watch_expiry_prefix(
            watch_stream_client,
            watch_registry,
            "space-watch:".to_string(),
            move |evt| {
                let mgr = Arc::clone(&watch_mgr);
                let lease_id = evt.lease_id;
                tokio::spawn(async move {
                    mgr.on_watch_expired(&lease_id).await;
                });
            },
        )
        .await;
    });

    let space_svc = SpaceServiceServer::new(SpaceServiceImpl::new(Arc::clone(&space_manager)));
    let space_participant_svc =
        ParticipantServiceServer::new(SpaceParticipantService::new(space_manager));

    info!(
        "  ✓ Space (split): listening on {actual_addr}, advertising {advertise_host}:{advertise_port}"
    );

    let registry_url = registry_addr.to_string();
    let advertise_host_owned = advertise_host.to_string();

    let register_fut = async move {
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

        match self_register(
            registry_client,
            "Space",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await
        {
            Ok(handle) => {
                info!(
                    "  ✓ Space: self-registered (capability: {}, lease: {})",
                    handle.capability_id(),
                    handle.lease_id()
                );
                std::future::pending::<()>().await;
            }
            Err(e) => {
                tracing::error!("self_register failed: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    let server_fut = Server::builder()
        .add_service(space_svc)
        .add_service(space_participant_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));

    info!("Djinn space ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}

// ── TransactionMgr alone ─────────────────────────────────────────────────────

/// Boot TransactionMgr alone. Reads `COORDIN8_BIND_ADDR` and `COORDIN8_REGISTRY`.
///
/// Requires `COORDIN8_REGISTRY` — TxnMgr needs a Registry to discover
/// LeaseMgr and to self-register into.
pub async fn run_txn() -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&bind_addr()).await?;
    let host = advertise_host();
    let registry_addr = std::env::var("COORDIN8_REGISTRY").map_err(|_| {
        anyhow::anyhow!("COORDIN8_REGISTRY must be set for split-mode TransactionMgr")
    })?;
    run_txn_on_listener(listener, &registry_addr, &host, 30).await
}

/// Boot TransactionMgr on a pre-bound [`TcpListener`], wired to a remote
/// LeaseMgr.
///
/// Uses `watch_expiry_prefix("txn:")` to drive `abort_expired(txn_id)` on
/// the local TxnManager when the remote LeaseMgr expires a transaction
/// lease — the txn id is carried in the `ExpiryEvent.resource_id` as the
/// suffix after the `txn:` prefix, matching the monolith's convention.
pub async fn run_txn_on_listener(
    listener: tokio::net::TcpListener,
    registry_addr: &str,
    advertise_host: &str,
    self_lease_ttl: u64,
) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let advertise_port = actual_addr.port();

    let remote_leasing = RemoteLeasing::connect(registry_addr).await?;
    let leasing: Arc<dyn Leasing> = Arc::new(remote_leasing);

    let txn_store: Arc<dyn TxnStore> = Arc::new(InMemoryTxnStore::new());
    let txn_manager = Arc::new(TxnManager::new(txn_store, Arc::clone(&leasing)));

    let lease_stream_client = coordin8_bootstrap::discover_lease_mgr(registry_addr).await?;
    let expiry_txn_mgr = Arc::clone(&txn_manager);
    let expiry_registry = registry_addr.to_string();
    tokio::spawn(async move {
        watch_expiry_prefix(
            lease_stream_client,
            expiry_registry,
            "txn:".to_string(),
            move |evt| {
                if let Some(txn_id) = evt.resource_id.strip_prefix("txn:") {
                    let mgr = Arc::clone(&expiry_txn_mgr);
                    let txn_id = txn_id.to_string();
                    tokio::spawn(async move {
                        let _ = mgr.abort_expired(&txn_id).await;
                    });
                }
            },
        )
        .await;
    });

    let txn_svc = TransactionServiceServer::new(TxnServiceImpl::new(txn_manager));

    info!(
        "  ✓ TransactionMgr (split): listening on {actual_addr}, advertising {advertise_host}:{advertise_port}"
    );

    let registry_url = registry_addr.to_string();
    let advertise_host_owned = advertise_host.to_string();

    let register_fut = async move {
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

        match self_register(
            registry_client,
            "TransactionMgr",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await
        {
            Ok(handle) => {
                info!(
                    "  ✓ TransactionMgr: self-registered (capability: {}, lease: {})",
                    handle.capability_id(),
                    handle.lease_id()
                );
                std::future::pending::<()>().await;
            }
            Err(e) => {
                tracing::error!("self_register failed: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    let server_fut = Server::builder()
        .add_service(txn_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));

    info!("Djinn txn ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}

// ── Proxy alone ──────────────────────────────────────────────────────────────

/// Boot Proxy alone. Reads `COORDIN8_BIND_ADDR` and `COORDIN8_REGISTRY`.
///
/// Requires `COORDIN8_REGISTRY` — Proxy resolves capability templates by
/// calling the Registry's `Lookup` RPC via `RemoteCapabilityResolver`.
pub async fn run_proxy() -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&bind_addr()).await?;
    let host = advertise_host();
    let registry_addr = std::env::var("COORDIN8_REGISTRY")
        .map_err(|_| anyhow::anyhow!("COORDIN8_REGISTRY must be set for split-mode Proxy"))?;
    run_proxy_on_listener(listener, &registry_addr, &host, 30).await
}

/// Boot Proxy on a pre-bound [`TcpListener`] against a remote Registry.
///
/// Resolution is done through `RemoteCapabilityResolver`, so every open and
/// every forwarded connection issues a Registry `Lookup` RPC — the same
/// "resolve at connection time" semantics as the monolith, but over the
/// wire. Reads `PROXY_BIND_HOST` / `PROXY_PORT_MIN` / `PROXY_PORT_MAX` via
/// `ProxyConfig::from_env`.
pub async fn run_proxy_on_listener(
    listener: tokio::net::TcpListener,
    registry_addr: &str,
    advertise_host: &str,
    self_lease_ttl: u64,
) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let advertise_port = actual_addr.port();

    let resolver = Arc::new(RemoteCapabilityResolver::connect(registry_addr).await?);
    let proxy_config = ProxyConfig::from_env();
    let proxy_manager = Arc::new(ProxyManager::new(resolver, proxy_config));

    let proxy_svc = ProxyServiceServer::new(ProxyServiceImpl::new(proxy_manager));

    info!(
        "  ✓ Proxy (split): listening on {actual_addr}, advertising {advertise_host}:{advertise_port}"
    );

    let registry_url = registry_addr.to_string();
    let advertise_host_owned = advertise_host.to_string();

    let register_fut = async move {
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

        match self_register(
            registry_client,
            "Proxy",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await
        {
            Ok(handle) => {
                info!(
                    "  ✓ Proxy: self-registered (capability: {}, lease: {})",
                    handle.capability_id(),
                    handle.lease_id()
                );
                std::future::pending::<()>().await;
            }
            Err(e) => {
                tracing::error!("self_register failed: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    let server_fut = Server::builder()
        .add_service(proxy_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));

    info!("Djinn proxy ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}
