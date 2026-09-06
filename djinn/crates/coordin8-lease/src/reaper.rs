use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tracing::{info, warn};

use coordin8_core::{LeaseReclaimed, ReclaimReason};

use crate::manager::LeaseManager;

/// Broadcasts lease reclamation events (expiry or cancel) to all active
/// cascade handlers and `WatchExpiry` subscribers.
pub type ExpiryBroadcast = broadcast::Sender<LeaseReclaimed>;

/// Background task: sweeps for expired leases on a fixed interval,
/// removes them from the store, and broadcasts expiry events.
///
/// `expiry_tx` should be the same sender `manager` was constructed with
/// (`manager.expiry_tx()`) — `cancel()` broadcasts on it directly too, so
/// cascade handlers see both expiry and explicit cancellation on one stream.
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
                    let _ = expiry_tx.send(LeaseReclaimed {
                        record,
                        reason: ReclaimReason::Expired,
                    });
                }
            }
            Err(e) => {
                warn!("reaper sweep failed: {}", e);
            }
        }
    }
}
