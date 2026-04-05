use async_trait::async_trait;
use aws_sdk_dynamodb::{Client, error::SdkError, types::AttributeValue};
use std::collections::HashMap;

use coordin8_core::{Error, RegistryEntry, RegistryStore, TransportConfig};

use crate::table::{ensure_registry_table, REGISTRY_LEASE_GSI, REGISTRY_TABLE};

pub struct DynamoRegistryStore {
    client: Client,
    table_name: String,
}

impl DynamoRegistryStore {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            table_name: REGISTRY_TABLE.to_string(),
        }
    }

    pub fn with_table(client: Client, table_name: impl Into<String>) -> Self {
        Self {
            client,
            table_name: table_name.into(),
        }
    }

    /// Create the backing table if it doesn't exist yet.
    pub async fn init(&self) -> Result<(), Error> {
        ensure_registry_table(&self.client, &self.table_name)
            .await
            .map_err(Error::Storage)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn entry_to_item(entry: &RegistryEntry) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();

    item.insert(
        "capability_id".to_string(),
        AttributeValue::S(entry.capability_id.clone()),
    );
    item.insert(
        "lease_id".to_string(),
        AttributeValue::S(entry.lease_id.clone()),
    );
    item.insert(
        "interface".to_string(),
        AttributeValue::S(entry.interface.clone()),
    );

    // attrs: HashMap<String, String> → DynamoDB Map
    let attrs_map: HashMap<String, AttributeValue> = entry
        .attrs
        .iter()
        .map(|(k, v)| (k.clone(), AttributeValue::S(v.clone())))
        .collect();
    item.insert("attrs".to_string(), AttributeValue::M(attrs_map));

    // transport: optional — omit fields entirely when None
    if let Some(transport) = &entry.transport {
        item.insert(
            "transport_type".to_string(),
            AttributeValue::S(transport.transport_type.clone()),
        );
        let config_map: HashMap<String, AttributeValue> = transport
            .config
            .iter()
            .map(|(k, v)| (k.clone(), AttributeValue::S(v.clone())))
            .collect();
        item.insert(
            "transport_config".to_string(),
            AttributeValue::M(config_map),
        );
    }

    item
}

fn entry_from_item(
    item: &HashMap<String, AttributeValue>,
) -> Result<RegistryEntry, Error> {
    let capability_id = item
        .get("capability_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing capability_id".into()))?
        .clone();

    let lease_id = item
        .get("lease_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing lease_id".into()))?
        .clone();

    let interface = item
        .get("interface")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing interface".into()))?
        .clone();

    let attrs = match item.get("attrs") {
        Some(AttributeValue::M(map)) => map
            .iter()
            .map(|(k, v)| {
                v.as_s()
                    .map(|s| (k.clone(), s.clone()))
                    .map_err(|_| Error::Storage(format!("attrs value for key '{k}' is not a string")))
            })
            .collect::<Result<HashMap<String, String>, Error>>()?,
        _ => HashMap::new(),
    };

    let transport = match (item.get("transport_type"), item.get("transport_config")) {
        (Some(AttributeValue::S(tt)), Some(AttributeValue::M(config_map))) => {
            let config = config_map
                .iter()
                .map(|(k, v)| {
                    v.as_s()
                        .map(|s| (k.clone(), s.clone()))
                        .map_err(|_| {
                            Error::Storage(format!(
                                "transport_config value for key '{k}' is not a string"
                            ))
                        })
                })
                .collect::<Result<HashMap<String, String>, Error>>()?;
            Some(TransportConfig {
                transport_type: tt.clone(),
                config,
            })
        }
        _ => None,
    };

    Ok(RegistryEntry {
        capability_id,
        lease_id,
        interface,
        attrs,
        transport,
    })
}

// ── trait implementation ──────────────────────────────────────────────────────

#[async_trait]
impl RegistryStore for DynamoRegistryStore {
    async fn insert(&self, entry: RegistryEntry) -> Result<(), Error> {
        let item = entry_to_item(&entry);

        self.client
            .put_item()
            .table_name(&self.table_name)
            .set_item(Some(item))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("put_item failed: {e}")))?;

        Ok(())
    }

    async fn update(&self, entry: RegistryEntry) -> Result<Option<RegistryEntry>, Error> {
        let item = entry_to_item(&entry);

        let result = self
            .client
            .put_item()
            .table_name(&self.table_name)
            .set_item(Some(item))
            .condition_expression("attribute_exists(capability_id)")
            .send()
            .await;

        match result {
            Ok(_) => Ok(Some(entry)),
            Err(SdkError::ServiceError(se))
                if se.err().is_conditional_check_failed_exception() =>
            {
                Ok(None)
            }
            Err(e) => Err(Error::Storage(format!("put_item (update) failed: {e}"))),
        }
    }

    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<RegistryEntry>, Error> {
        // Step 1: Query the GSI to find the capability_id for this lease_id
        let resp = self
            .client
            .query()
            .table_name(&self.table_name)
            .index_name(REGISTRY_LEASE_GSI)
            .key_condition_expression("lease_id = :lid")
            .expression_attribute_values(":lid", AttributeValue::S(lease_id.to_string()))
            .limit(1)
            .send()
            .await
            .map_err(|e| Error::Storage(format!("query (lease GSI) failed: {e}")))?;

        let item = match resp.items.and_then(|items| items.into_iter().next()) {
            Some(item) => item,
            None => return Ok(None),
        };

        let capability_id = item
            .get("capability_id")
            .and_then(|v| v.as_s().ok())
            .ok_or_else(|| Error::Storage("GSI item missing capability_id".into()))?
            .clone();

        // Reconstruct entry from the GSI result (projection = ALL, so all fields present)
        let entry = entry_from_item(&item)?;

        // Step 2: Delete by PK
        self.client
            .delete_item()
            .table_name(&self.table_name)
            .key("capability_id", AttributeValue::S(capability_id))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;

        Ok(Some(entry))
    }

    async fn get(&self, capability_id: &str) -> Result<Option<RegistryEntry>, Error> {
        let resp = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("capability_id", AttributeValue::S(capability_id.to_string()))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("get_item failed: {e}")))?;

        match resp.item {
            None => Ok(None),
            Some(item) => Ok(Some(entry_from_item(&item)?)),
        }
    }

    async fn get_by_lease(&self, lease_id: &str) -> Result<Option<RegistryEntry>, Error> {
        let resp = self
            .client
            .query()
            .table_name(&self.table_name)
            .index_name(REGISTRY_LEASE_GSI)
            .key_condition_expression("lease_id = :lid")
            .expression_attribute_values(":lid", AttributeValue::S(lease_id.to_string()))
            .limit(1)
            .send()
            .await
            .map_err(|e| Error::Storage(format!("query (lease GSI) failed: {e}")))?;

        match resp.items.and_then(|items| items.into_iter().next()) {
            None => Ok(None),
            Some(item) => Ok(Some(entry_from_item(&item)?)),
        }
    }

    async fn list_all(&self) -> Result<Vec<RegistryEntry>, Error> {
        let mut entries = Vec::new();
        let mut last_key: Option<HashMap<String, AttributeValue>> = None;

        loop {
            let mut req = self.client.scan().table_name(&self.table_name);

            if let Some(ref lek) = last_key {
                req = req.set_exclusive_start_key(Some(lek.clone()));
            }

            let resp = req
                .send()
                .await
                .map_err(|e| Error::Storage(format!("scan failed: {e}")))?;

            for item in resp.items.unwrap_or_default() {
                entries.push(entry_from_item(&item)?);
            }

            last_key = resp.last_evaluated_key;
            if last_key.is_none() {
                break;
            }
        }

        Ok(entries)
    }
}

// ── integration tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::make_dynamo_client;
    use aws_sdk_dynamodb::Client;
    use uuid::Uuid;

    async fn setup() -> (DynamoRegistryStore, String, Client) {
        std::env::set_var("DYNAMODB_ENDPOINT", "http://localhost:4566");
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
        std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");

        let client = make_dynamo_client().await;
        let table_name = format!("coordin8_registry_test_{}", Uuid::new_v4());

        let store = DynamoRegistryStore::with_table(client.clone(), &table_name);
        store.init().await.expect("table creation failed");

        (store, table_name, client)
    }

    async fn teardown(client: &Client, table_name: &str) {
        let _ = client
            .delete_table()
            .table_name(table_name)
            .send()
            .await;
    }

    fn make_entry(capability_id: &str, lease_id: &str) -> RegistryEntry {
        let mut attrs = HashMap::new();
        attrs.insert("host".to_string(), "localhost".to_string());
        attrs.insert("port".to_string(), "9000".to_string());

        let mut transport_config = HashMap::new();
        transport_config.insert("address".to_string(), "localhost:9000".to_string());

        RegistryEntry {
            capability_id: capability_id.to_string(),
            lease_id: lease_id.to_string(),
            interface: "Greeter".to_string(),
            attrs,
            transport: Some(TransportConfig {
                transport_type: "grpc".to_string(),
                config: transport_config,
            }),
        }
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn insert_and_get_with_transport() {
        let (store, table_name, client) = setup().await;

        let entry = make_entry("cap-1", "lease-1");
        store.insert(entry.clone()).await.unwrap();

        let fetched = store.get("cap-1").await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.capability_id, "cap-1");
        assert_eq!(fetched.lease_id, "lease-1");
        assert_eq!(fetched.interface, "Greeter");
        assert_eq!(fetched.attrs.get("host").unwrap(), "localhost");
        let transport = fetched.transport.unwrap();
        assert_eq!(transport.transport_type, "grpc");
        assert_eq!(transport.config.get("address").unwrap(), "localhost:9000");

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn insert_with_none_transport() {
        let (store, table_name, client) = setup().await;

        let entry = RegistryEntry {
            capability_id: "cap-no-transport".to_string(),
            lease_id: "lease-2".to_string(),
            interface: "Notifier".to_string(),
            attrs: HashMap::new(),
            transport: None,
        };
        store.insert(entry).await.unwrap();

        let fetched = store.get("cap-no-transport").await.unwrap().unwrap();
        assert!(fetched.transport.is_none());

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn update_existing() {
        let (store, table_name, client) = setup().await;

        let entry = make_entry("cap-update", "lease-3");
        store.insert(entry).await.unwrap();

        let mut updated_attrs = HashMap::new();
        updated_attrs.insert("host".to_string(), "remotehost".to_string());
        let updated = RegistryEntry {
            capability_id: "cap-update".to_string(),
            lease_id: "lease-3".to_string(),
            interface: "Greeter".to_string(),
            attrs: updated_attrs,
            transport: None,
        };

        let result = store.update(updated).await.unwrap();
        assert!(result.is_some());
        let returned = result.unwrap();
        assert_eq!(returned.attrs.get("host").unwrap(), "remotehost");

        let fetched = store.get("cap-update").await.unwrap().unwrap();
        assert_eq!(fetched.attrs.get("host").unwrap(), "remotehost");

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn update_non_existent_returns_none() {
        let (store, table_name, client) = setup().await;

        let entry = make_entry("does-not-exist", "lease-x");
        let result = store.update(entry).await.unwrap();
        assert!(result.is_none());

        // Should not have been inserted
        let fetched = store.get("does-not-exist").await.unwrap();
        assert!(fetched.is_none());

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn remove_by_lease() {
        let (store, table_name, client) = setup().await;

        let entry = make_entry("cap-remove", "lease-rm");
        store.insert(entry).await.unwrap();

        let removed = store.remove_by_lease("lease-rm").await.unwrap();
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().capability_id, "cap-remove");

        let fetched = store.get("cap-remove").await.unwrap();
        assert!(fetched.is_none());

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn remove_by_lease_non_existent_returns_none() {
        let (store, table_name, client) = setup().await;

        let result = store.remove_by_lease("no-such-lease").await.unwrap();
        assert!(result.is_none());

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn get_by_lease() {
        let (store, table_name, client) = setup().await;

        let entry = make_entry("cap-by-lease", "lease-lookup");
        store.insert(entry).await.unwrap();

        let found = store.get_by_lease("lease-lookup").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().capability_id, "cap-by-lease");

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn list_all() {
        let (store, table_name, client) = setup().await;

        store.insert(make_entry("cap-a", "lease-a")).await.unwrap();
        store.insert(make_entry("cap-b", "lease-b")).await.unwrap();
        store.insert(make_entry("cap-c", "lease-c")).await.unwrap();

        let all = store.list_all().await.unwrap();
        assert_eq!(all.len(), 3);

        let ids: Vec<String> = all.iter().map(|e| e.capability_id.clone()).collect();
        assert!(ids.contains(&"cap-a".to_string()));
        assert!(ids.contains(&"cap-b".to_string()));
        assert!(ids.contains(&"cap-c".to_string()));

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn update_changes_lease_id() {
        let (store, table_name, client) = setup().await;

        let entry = make_entry("cap-lease-change", "old-lease");
        store.insert(entry).await.unwrap();

        // Update with a new lease_id
        let updated = RegistryEntry {
            capability_id: "cap-lease-change".to_string(),
            lease_id: "new-lease".to_string(),
            interface: "Greeter".to_string(),
            attrs: HashMap::new(),
            transport: None,
        };
        store.update(updated).await.unwrap();

        // Old lease_id should no longer resolve
        let old_lookup = store.get_by_lease("old-lease").await.unwrap();
        assert!(old_lookup.is_none());

        // New lease_id should resolve
        let new_lookup = store.get_by_lease("new-lease").await.unwrap();
        assert!(new_lookup.is_some());
        assert_eq!(new_lookup.unwrap().capability_id, "cap-lease-change");

        teardown(&client, &table_name).await;
    }
}
