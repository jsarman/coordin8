use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use uuid::Uuid;

use coordin8_core::{checked_expiry, Error, LeaseRecord, LeaseStore};

fn expiry_for(from: chrono::DateTime<Utc>, ttl_secs: u64) -> Result<chrono::DateTime<Utc>, Error> {
    if ttl_secs == coordin8_core::LEASE_FOREVER {
        // FOREVER: far-future sentinel.
        return Ok(chrono::DateTime::<Utc>::MAX_UTC);
    }
    checked_expiry(from, ttl_secs)
        .ok_or_else(|| Error::Storage(format!("lease ttl out of range: {ttl_secs}s")))
}

/// Thread-safe in-memory lease store. Zero dependencies. Dev and test use.
pub struct InMemoryLeaseStore {
    leases: DashMap<String, LeaseRecord>,
}

impl InMemoryLeaseStore {
    pub fn new() -> Self {
        Self {
            leases: DashMap::new(),
        }
    }
}

impl Default for InMemoryLeaseStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LeaseStore for InMemoryLeaseStore {
    async fn create(&self, resource_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error> {
        let now = Utc::now();
        let expires_at = expiry_for(now, ttl_secs)?;
        let record = LeaseRecord {
            lease_id: Uuid::new_v4().to_string(),
            resource_id: resource_id.to_string(),
            granted_at: now,
            expires_at,
            ttl_seconds: ttl_secs,
        };
        self.leases.insert(record.lease_id.clone(), record.clone());
        Ok(record)
    }

    async fn renew(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error> {
        let new_expiry = expiry_for(Utc::now(), ttl_secs)?;
        let mut entry = self
            .leases
            .get_mut(lease_id)
            .ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;
        entry.expires_at = new_expiry;
        entry.ttl_seconds = ttl_secs;
        Ok(entry.clone())
    }

    async fn cancel(&self, lease_id: &str) -> Result<(), Error> {
        self.leases.remove(lease_id);
        Ok(())
    }

    async fn get(&self, lease_id: &str) -> Result<Option<LeaseRecord>, Error> {
        Ok(self.leases.get(lease_id).map(|r| r.clone()))
    }

    async fn get_by_resource(&self, resource_id: &str) -> Result<Option<LeaseRecord>, Error> {
        Ok(self
            .leases
            .iter()
            .find(|r| r.resource_id == resource_id)
            .map(|r| r.clone()))
    }

    async fn list_expired(&self) -> Result<Vec<LeaseRecord>, Error> {
        Ok(self
            .leases
            .iter()
            .filter(|r| r.is_expired())
            .map(|r| r.clone())
            .collect())
    }

    async fn remove_if_unchanged(&self, record: &LeaseRecord) -> Result<bool, Error> {
        Ok(self
            .leases
            .remove_if(&record.lease_id, |_, cur| {
                cur.expires_at == record.expires_at
            })
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn grant_and_get() {
        let store = InMemoryLeaseStore::new();
        let record = store.create("worker-1", 30).await.unwrap();
        assert_eq!(record.resource_id, "worker-1");
        assert!(!record.is_expired());

        let fetched = store.get(&record.lease_id).await.unwrap();
        assert!(fetched.is_some());
    }

    #[tokio::test]
    async fn cancel_removes_lease() {
        let store = InMemoryLeaseStore::new();
        let record = store.create("worker-2", 30).await.unwrap();
        store.cancel(&record.lease_id).await.unwrap();
        assert!(store.get(&record.lease_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_lease_shows_up_in_list() {
        let store = InMemoryLeaseStore::new();
        let record = store.create("worker-3", 1).await.unwrap(); // 1s TTL
        sleep(Duration::from_millis(1100)).await;
        let expired = store.list_expired().await.unwrap();
        assert!(expired.iter().any(|r| r.lease_id == record.lease_id));
    }

    #[tokio::test]
    async fn forever_lease_never_expires() {
        let store = InMemoryLeaseStore::new();
        let record = store
            .create("worker-forever", coordin8_core::LEASE_FOREVER)
            .await
            .unwrap();
        assert_eq!(record.ttl_seconds, coordin8_core::LEASE_FOREVER);
        assert!(!record.is_expired());
        sleep(Duration::from_millis(10)).await;
        let expired = store.list_expired().await.unwrap();
        assert!(!expired.iter().any(|r| r.lease_id == record.lease_id));
    }

    #[tokio::test]
    async fn renewed_between_list_and_remove_is_not_removed() {
        let store = InMemoryLeaseStore::new();
        let record = store.create("racer", 1).await.unwrap();
        sleep(Duration::from_millis(1100)).await;
        let expired = store.list_expired().await.unwrap();
        assert_eq!(expired.len(), 1);

        store.renew(&record.lease_id, 60).await.unwrap();
        assert!(!store.remove_if_unchanged(&expired[0]).await.unwrap());
        assert!(store.get(&record.lease_id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn remove_if_unchanged_removes_unchanged_lease() {
        let store = InMemoryLeaseStore::new();
        let record = store.create("plain", 1).await.unwrap();
        sleep(Duration::from_millis(1100)).await;
        let expired = store.list_expired().await.unwrap();
        assert!(store.remove_if_unchanged(&expired[0]).await.unwrap());
        assert!(store.get(&record.lease_id).await.unwrap().is_none());
        // Already gone.
        assert!(!store.remove_if_unchanged(&expired[0]).await.unwrap());
    }

    #[tokio::test]
    async fn huge_ttl_negotiates_to_forever_and_does_not_panic() {
        let cfg = coordin8_core::LeaseConfig {
            max_ttl: None,
            preferred_ttl: 300,
        };
        let granted = cfg.negotiate(100_000_000_000_000_000);
        assert_eq!(granted, coordin8_core::LEASE_FOREVER);
        let granted_neg = cfg.negotiate(u64::MAX - 1);
        assert_eq!(granted_neg, coordin8_core::LEASE_FOREVER);

        let store = InMemoryLeaseStore::new();
        let record = store.create("huge", granted).await.unwrap();
        assert!(!record.is_expired());
        store.renew(&record.lease_id, granted).await.unwrap();
        // Direct, un-negotiated huge TTLs error instead of panicking.
        assert!(store.create("bad", 100_000_000_000_000_000).await.is_err());
        assert!(store.create("bad", u64::MAX - 1).await.is_err());
        assert!(store.renew(&record.lease_id, u64::MAX - 1).await.is_err());
    }
}
