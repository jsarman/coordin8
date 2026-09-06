use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use coordin8_core::{
    Error, LeaseConfig, LeaseReclaimed, LeaseRecord, LeaseStore, Leasing, ReclaimReason,
};

use crate::reaper::ExpiryBroadcast;

/// Coordinates lease operations. Wraps the backing store with business logic.
///
/// Owned by whichever service embeds it (Registry, Space, EventMgr,
/// TransactionMgr each construct their own) and shared (Arc) across that
/// service's gRPC handlers and its own reaper task.
pub struct LeaseManager {
    store: Arc<dyn LeaseStore>,
    config: LeaseConfig,
    /// Broadcasts a [`LeaseReclaimed`] on both natural expiry (the reaper,
    /// see `reaper.rs`) and explicit `cancel()` below — cascade handlers
    /// react to both the same way, matching Jini's stance that cancel and
    /// expiry differ in promptness/intent, not in whether cleanup happens.
    expiry_tx: ExpiryBroadcast,
}

impl LeaseManager {
    pub fn new(
        store: Arc<dyn LeaseStore>,
        config: LeaseConfig,
        expiry_tx: ExpiryBroadcast,
    ) -> Self {
        Self {
            store,
            config,
            expiry_tx,
        }
    }

    pub async fn get(&self, lease_id: &str) -> Result<Option<LeaseRecord>, Error> {
        self.store.get(lease_id).await
    }

    /// Called by the reaper. Returns expired leases and removes them from the store.
    pub async fn drain_expired(&self) -> Result<Vec<LeaseRecord>, Error> {
        let expired = self.store.list_expired().await?;
        for record in &expired {
            self.store.remove(&record.lease_id).await?;
        }
        Ok(expired)
    }

    /// The broadcast channel this manager's reaper (and `cancel()`) publish
    /// [`LeaseReclaimed`] events on. Cascade handlers subscribe to this
    /// directly — entirely in-process, no gRPC hop.
    pub fn expiry_tx(&self) -> ExpiryBroadcast {
        self.expiry_tx.clone()
    }
}

#[async_trait]
impl Leasing for LeaseManager {
    async fn grant(&self, resource_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error> {
        let granted_ttl = self.config.negotiate(ttl_secs);
        let record = self.store.create(resource_id, granted_ttl).await?;
        debug!(
            lease_id = %record.lease_id,
            resource_id = %record.resource_id,
            requested_ttl = ttl_secs,
            granted_ttl,
            expires_at = %record.expires_at,
            "lease granted"
        );
        Ok(record)
    }

    async fn renew(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error> {
        let record = self
            .store
            .get(lease_id)
            .await?
            .ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;

        if record.is_expired() {
            debug!(lease_id, "renew rejected — lease already expired");
            return Err(Error::LeaseExpired(lease_id.to_string()));
        }

        let granted_ttl = self.config.negotiate(ttl_secs);
        let record = self.store.renew(lease_id, granted_ttl).await?;
        debug!(
            lease_id = %record.lease_id,
            resource_id = %record.resource_id,
            requested_ttl = ttl_secs,
            granted_ttl,
            expires_at = %record.expires_at,
            "lease renewed"
        );
        Ok(record)
    }

    async fn cancel(&self, lease_id: &str) -> Result<(), Error> {
        // Fetch before cancel so we can log it and broadcast the full record.
        let record = self.store.get(lease_id).await?;
        self.store.cancel(lease_id).await?;
        let resource_id = record
            .as_ref()
            .map_or("<unknown>", |r| r.resource_id.as_str());
        debug!(lease_id, resource_id, "lease cancelled");
        if let Some(record) = record {
            // Receivers that have dropped are fine — send returns Err only
            // when there are no receivers, which is non-fatal.
            let _ = self.expiry_tx.send(LeaseReclaimed {
                record,
                reason: ReclaimReason::Cancelled,
            });
        }
        Ok(())
    }
}
