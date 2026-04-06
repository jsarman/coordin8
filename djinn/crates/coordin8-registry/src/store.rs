use std::sync::Arc;

use coordin8_core::{Error, RegistryEntry, RegistryStore};

use crate::matcher::{matches, parse_template};

/// Registry business logic layer. Wraps the backing store.
pub struct RegistryIndex {
    store: Arc<dyn RegistryStore>,
}

impl RegistryIndex {
    pub fn new(store: Arc<dyn RegistryStore>) -> Self {
        Self { store }
    }

    pub async fn register(&self, entry: RegistryEntry) -> Result<(), Error> {
        self.store.insert(entry).await
    }

    /// Update an existing entry in-place. Returns None if capability_id not found.
    pub async fn update(&self, entry: RegistryEntry) -> Result<Option<RegistryEntry>, Error> {
        self.store.update(entry).await
    }

    /// Get an entry by capability_id.
    pub async fn get(&self, capability_id: &str) -> Result<Option<RegistryEntry>, Error> {
        self.store.get(capability_id).await
    }

    /// Get an entry by its lease_id.
    pub async fn get_by_lease(&self, lease_id: &str) -> Result<Option<RegistryEntry>, Error> {
        self.store.get_by_lease(lease_id).await
    }

    pub async fn unregister_by_lease(
        &self,
        lease_id: &str,
    ) -> Result<Option<RegistryEntry>, Error> {
        self.store.remove_by_lease(lease_id).await
    }

    pub async fn lookup(
        &self,
        template: &std::collections::HashMap<String, String>,
    ) -> Result<Option<RegistryEntry>, Error> {
        let ops = parse_template(template);
        let all = self.store.list_all().await?;
        Ok(all.into_iter().find(|e| {
            let mut combined = e.attrs.clone();
            combined.insert("interface".to_string(), e.interface.clone());
            matches(&ops, &combined)
        }))
    }

    pub async fn lookup_all(
        &self,
        template: &std::collections::HashMap<String, String>,
    ) -> Result<Vec<RegistryEntry>, Error> {
        let ops = parse_template(template);
        let all = self.store.list_all().await?;
        Ok(all
            .into_iter()
            .filter(|e| {
                let mut combined = e.attrs.clone();
                combined.insert("interface".to_string(), e.interface.clone());
                matches(&ops, &combined)
            })
            .collect())
    }
}
