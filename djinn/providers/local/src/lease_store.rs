use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use uuid::Uuid;

use coordin8_core::{Error, LeaseRecord, LeaseStore};

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
        let expires_at = if ttl_secs == coordin8_core::LEASE_FOREVER {
            // FOREVER: set far-future sentinel so list_expired never picks it up.
            chrono::DateTime::<Utc>::MAX_UTC
        } else {
            now + chrono::Duration::seconds(ttl_secs as i64)
        };
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
        let mut entry = self
            .leases
            .get_mut(lease_id)
            .ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;
        if ttl_secs == coordin8_core::LEASE_FOREVER {
            entry.expires_at = chrono::DateTime::<Utc>::MAX_UTC;
        } else {
            entry.expires_at = Utc::now() + chrono::Duration::seconds(ttl_secs as i64);
        }
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
        let now = Utc::now();
        Ok(self
            .leases
            .iter()
            .filter(|r| r.expires_at <= now)
            .map(|r| r.clone())
            .collect())
    }

    async fn remove(&self, lease_id: &str) -> Result<(), Error> {
        self.leases.remove(lease_id);
        Ok(())
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
        let record = store.create("worker-forever", 0).await.unwrap(); // ttl=0 → FOREVER
        assert_eq!(record.ttl_seconds, 0);
        assert!(!record.is_expired());
        sleep(Duration::from_millis(10)).await;
        let expired = store.list_expired().await.unwrap();
        assert!(!expired.iter().any(|r| r.lease_id == record.lease_id));
    }
}
