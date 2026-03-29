use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::broadcast;
use tonic::transport::Server;
use tracing::info;

use coordin8_lease::{LeaseManager, LeaseServiceImpl};
use coordin8_proto::coordin8::lease_service_server::LeaseServiceServer;
use coordin8_proto::coordin8::registry_service_server::RegistryServiceServer;
use coordin8_provider_local::{InMemoryLeaseStore, InMemoryRegistryStore};
use coordin8_registry::service::RegistryBroadcast;
use coordin8_registry::{store::RegistryIndex, RegistryServiceImpl};

/// Coordin8 boot sequence:
///
///   Layer 0 → Provider (storage)
///   Layer 1 → LeaseMgr (bedrock)
///   Layer 2 → Registry (peers with Space, neither depends on the other)
///   Layer 3 → TransactionMgr (Phase 7)
///   Layer 4 → Space (Phase 5)
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
    let lease_store = Arc::new(InMemoryLeaseStore::new());
    let registry_store = Arc::new(InMemoryRegistryStore::new());
    info!("  ✓ Provider: local (in-memory)");

    // ── Layer 1: LeaseMgr ────────────────────────────────────────────────────
    let (expiry_tx, _) = broadcast::channel::<coordin8_core::LeaseRecord>(256);
    let lease_manager = Arc::new(LeaseManager::new(lease_store));

    // Background reaper — sweeps for expired leases every second
    let reaper_manager = Arc::clone(&lease_manager);
    let reaper_tx = expiry_tx.clone();
    tokio::spawn(async move {
        coordin8_lease::reaper::run_reaper(reaper_manager, reaper_tx, Duration::from_secs(1)).await;
    });
    info!("  ✓ LeaseMgr: ready");

    // ── Layer 2: Registry ────────────────────────────────────────────────────
    let (registry_tx, _): (RegistryBroadcast, _) = broadcast::channel(256);
    let registry_index = Arc::new(RegistryIndex::new(registry_store));
    info!("  ✓ Registry: ready");

    // ── gRPC servers ─────────────────────────────────────────────────────────
    let lease_addr = "0.0.0.0:9001".parse()?;
    let registry_addr = "0.0.0.0:9002".parse()?;

    let lease_svc =
        LeaseServiceServer::new(LeaseServiceImpl::new(Arc::clone(&lease_manager), expiry_tx));
    let registry_svc = RegistryServiceServer::new(RegistryServiceImpl::new(
        registry_index,
        Arc::clone(&lease_manager),
        registry_tx,
    ));

    info!("  ✓ LeaseMgr:  listening on {}", lease_addr);
    info!("  ✓ Registry:  listening on {}", registry_addr);
    info!("Djinn ready.");

    // Run both gRPC servers concurrently
    tokio::try_join!(
        Server::builder().add_service(lease_svc).serve(lease_addr),
        Server::builder()
            .add_service(registry_svc)
            .serve(registry_addr),
    )?;

    Ok(())
}
