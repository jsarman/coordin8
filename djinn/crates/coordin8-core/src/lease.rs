use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub lease_id: String,
    pub resource_id: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl LeaseRecord {
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }
}

/// Backing store for lease data. Implemented by providers (local, AWS, etc.).
#[async_trait]
pub trait LeaseStore: Send + Sync {
    async fn create(&self, resource_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn renew(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn cancel(&self, lease_id: &str) -> Result<(), Error>;
    async fn get(&self, lease_id: &str) -> Result<Option<LeaseRecord>, Error>;
    async fn get_by_resource(&self, resource_id: &str) -> Result<Option<LeaseRecord>, Error>;
    /// Returns all records whose expires_at is in the past.
    async fn list_expired(&self) -> Result<Vec<LeaseRecord>, Error>;
    async fn remove(&self, lease_id: &str) -> Result<(), Error>;
}
