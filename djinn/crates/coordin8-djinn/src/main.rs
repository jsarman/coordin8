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

/// Coordin8 boot sequence:
///
///   Layer 0 → Provider (storage)
///   Layer 1 → LeaseMgr (bedrock)
///   Layer 2 → Registry
///   Layer 3 → Proxy (depends on Registry)
///   Layer 4 → TransactionMgr (Phase 7)
///   Layer 5 → Space (Phase 5)
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("coordin8=info".parse()?),
        )
        .init();

    info!("Djinn starting...");

    // ── Layer 0: Provider ────────────────────────────────────────────────────
    let provider = std::env::var("COORDIN8_PROVIDER").unwrap_or_else(|_| "local".into());

    // These are Arc<dyn TraitName> — the type erases the concrete provider
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

    // Listen for lease expirations relevant to Registry entries.
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
                            event_type: 1, // EXPIRED
                            entry,
                        });
                }
            }
        }
    });
    info!("  ✓ Registry: ready");

    // ── Layer 2b: EventMgr (peer to Registry) ────────────────────────────────
    let (event_tx, _) = broadcast::channel::<coordin8_core::EventRecord>(256);
    let event_manager = Arc::new(EventManager::new(
        event_store,
        Arc::clone(&lease_manager),
        event_tx,
    ));

    // Listen for lease expirations relevant to event subscriptions.
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

    // Listen for lease expirations relevant to transactions.
    // Lease expiry = auto-abort (per Jini spec: "let the lease expire and
    // the transaction auto-aborts").
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

    // ── Layer 2c: Space (peer with Registry + EventMgr) ─────────────────────
    let (space_tuple_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    let (space_expiry_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    let space_manager = Arc::new(SpaceManager::new(
        space_store,
        Arc::clone(&lease_manager),
        space_tuple_tx,
        space_expiry_tx,
    ));

    // Listen for lease expirations relevant to Space tuples and watches.
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
