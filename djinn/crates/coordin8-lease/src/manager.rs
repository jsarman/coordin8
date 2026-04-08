use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use coordin8_core::{Error, LeaseConfig, LeaseRecord, LeaseStore, Leasing};

/// Coordinates lease operations. Wraps the backing store with business logic.
///
/// Owned by the Djinn and shared (Arc) across the gRPC service and reaper.
pub struct LeaseManager {
    store: Arc<dyn LeaseStore>,
    config: LeaseConfig,
}

impl LeaseManager {
    pub fn new(store: Arc<dyn LeaseStore>, config: LeaseConfig) -> Self {
        Self { store, config }
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
        // Fetch before cancel so we can log the resource_id
        let resource_id = self
            .store
            .get(lease_id)
            .await?
            .map(|r| r.resource_id)
            .unwrap_or_else(|| "<unknown>".to_string());
        self.store.cancel(lease_id).await?;
        debug!(lease_id, resource_id, "lease cancelled");
        Ok(())
    }
}
