//! In-process [`CapabilityResolver`] that matches templates against a
//! [`RegistryStore`] directly. This is the monolith path — Proxy and Registry
//! share the same process and the same store.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use coordin8_core::{CapabilityResolver, Error, RegistryEntry, RegistryStore};
use coordin8_registry::matcher::{matches, parse_template};

/// Resolves templates by scanning a local [`RegistryStore`] and applying the
/// same matcher the Registry service uses server-side.
pub struct LocalCapabilityResolver {
    store: Arc<dyn RegistryStore>,
}

impl LocalCapabilityResolver {
    pub fn new(store: Arc<dyn RegistryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl CapabilityResolver for LocalCapabilityResolver {
    async fn resolve(
        &self,
        template: &HashMap<String, String>,
    ) -> Result<Option<RegistryEntry>, Error> {
        let ops = parse_template(template);
        let entries = self.store.list_all().await?;

        Ok(entries.into_iter().find(|e| {
            let mut all_attrs = e.attrs.clone();
            all_attrs.insert("interface".to_string(), e.interface.clone());
            matches(&ops, &all_attrs)
        }))
    }
}
