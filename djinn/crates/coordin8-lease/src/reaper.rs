use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tracing::{info, warn};

use coordin8_core::LeaseRecord;

use crate::manager::LeaseManager;

/// Broadcasts expiry events to all active WatchExpiry streams.
pub type ExpiryBroadcast = broadcast::Sender<LeaseRecord>;

/// Background task: sweeps for expired leases on a fixed interval,
/// removes them from the store, and broadcasts expiry events.
pub async fn run_reaper(
    manager: Arc<LeaseManager>,
    expiry_tx: ExpiryBroadcast,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        match manager.drain_expired().await {
            Ok(expired) => {
                for record in expired {
                    info!(
                        lease_id = %record.lease_id,
                        resource_id = %record.resource_id,
                        "lease expired"
                    );
                    // Receivers that have dropped are fine — send returns Err only
                    // when there are no receivers, which is non-fatal.
                    let _ = expiry_tx.send(record);
                }
            }
            Err(e) => {
                warn!("reaper sweep failed: {}", e);
            }
        }
    }
}
