use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportConfig {
    pub transport_type: String,
    pub config: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub capability_id: String,
    pub lease_id: String,
    pub interface: String,
    pub attrs: HashMap<String, String>,
    pub transport: Option<TransportConfig>,
}

/// Backing store for registry entries. Implemented by providers.
#[async_trait]
pub trait RegistryStore: Send + Sync {
    async fn insert(&self, entry: RegistryEntry) -> Result<(), Error>;
    /// Update an existing entry in-place. Returns the updated entry.
    async fn update(&self, entry: RegistryEntry) -> Result<Option<RegistryEntry>, Error>;
    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<RegistryEntry>, Error>;
    async fn get(&self, capability_id: &str) -> Result<Option<RegistryEntry>, Error>;
    /// Find an entry by its lease_id.
    async fn get_by_lease(&self, lease_id: &str) -> Result<Option<RegistryEntry>, Error>;
    async fn list_all(&self) -> Result<Vec<RegistryEntry>, Error>;
}

/// Narrow seam used by callers that only need to turn a capability template
/// into the first matching `RegistryEntry`. This is the piece of Registry
/// the Proxy layer talks to, factored out so it can be implemented both
/// locally (wrapping a `RegistryStore`) and remotely (over gRPC to a
/// Registry service) — the Jini lease-as-interface pattern, applied to
/// capability resolution.
#[async_trait]
pub trait CapabilityResolver: Send + Sync {
    /// Resolve a template to the first matching entry. Returns `None` if no
    /// capability currently matches.
    async fn resolve(
        &self,
        template: &HashMap<String, String>,
    ) -> Result<Option<RegistryEntry>, Error>;
}
