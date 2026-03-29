use async_trait::async_trait;
use dashmap::DashMap;

use coordin8_core::{Error, RegistryEntry, RegistryStore};

/// Thread-safe in-memory registry store. Dev and test use.
pub struct InMemoryRegistryStore {
    entries: DashMap<String, RegistryEntry>,
    // Index: lease_id → capability_id for fast expiry cleanup
    lease_index: DashMap<String, String>,
}

impl InMemoryRegistryStore {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            lease_index: DashMap::new(),
        }
    }
}

impl Default for InMemoryRegistryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RegistryStore for InMemoryRegistryStore {
    async fn insert(&self, entry: RegistryEntry) -> Result<(), Error> {
        self.lease_index
            .insert(entry.lease_id.clone(), entry.capability_id.clone());
        self.entries.insert(entry.capability_id.clone(), entry);
        Ok(())
    }

    async fn remove_by_lease(&self, lease_id: &str) -> Result<(), Error> {
        if let Some((_, capability_id)) = self.lease_index.remove(lease_id) {
            self.entries.remove(&capability_id);
        }
        Ok(())
    }

    async fn get(&self, capability_id: &str) -> Result<Option<RegistryEntry>, Error> {
        Ok(self.entries.get(capability_id).map(|e| e.clone()))
    }

    async fn list_all(&self) -> Result<Vec<RegistryEntry>, Error> {
        Ok(self.entries.iter().map(|e| e.clone()).collect())
    }
}
