use async_trait::async_trait;
use aws_sdk_dynamodb::{
    Client,
    types::AttributeValue,
};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use coordin8_core::{Error, LeaseRecord, LeaseStore, LEASE_FOREVER};

use crate::table::{ensure_lease_table, LEASE_TABLE, RESOURCE_GSI};

pub struct DynamoLeaseStore {
    client: Client,
    table_name: String,
}

impl DynamoLeaseStore {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            table_name: LEASE_TABLE.to_string(),
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
        ensure_lease_table(&self.client, &self.table_name)
            .await
            .map_err(Error::Storage)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Convert an expiry timestamp to a DynamoDB TTL epoch value.
///
/// For FOREVER leases we must NOT store epoch 0 — DynamoDB's native TTL reaper
/// will eventually garbage-collect items whose TTL is in the past (epoch 0 is
/// 1970-01-01). Instead we omit the TTL attribute entirely via `None`.
fn ttl_epoch(expires_at: &DateTime<Utc>, ttl_seconds: u64) -> Option<i64> {
    if ttl_seconds == LEASE_FOREVER {
        None
    } else {
        Some(expires_at.timestamp())
    }
}

fn record_from_item(
    item: &std::collections::HashMap<String, AttributeValue>,
) -> Result<LeaseRecord, Error> {
    let lease_id = item
        .get("lease_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing lease_id".into()))?
        .clone();
    let resource_id = item
        .get("resource_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing resource_id".into()))?
        .clone();
    let granted_at: DateTime<Utc> = item
        .get("granted_at")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing granted_at".into()))
        .and_then(|s| {
            s.parse::<DateTime<Utc>>()
                .map_err(|e| Error::Storage(format!("bad granted_at: {e}")))
        })?;
    let expires_at: DateTime<Utc> = item
        .get("expires_at")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing expires_at".into()))
        .and_then(|s| {
            s.parse::<DateTime<Utc>>()
                .map_err(|e| Error::Storage(format!("bad expires_at: {e}")))
        })?;
    let ttl_seconds: u64 = item
        .get("ttl_seconds")
        .and_then(|v| v.as_n().ok())
        .ok_or_else(|| Error::Storage("missing ttl_seconds".into()))
        .and_then(|s| {
            s.parse::<u64>()
                .map_err(|e| Error::Storage(format!("bad ttl_seconds: {e}")))
        })?;
    Ok(LeaseRecord {
        lease_id,
        resource_id,
        granted_at,
        expires_at,
        ttl_seconds,
    })
}

// ── trait implementation ──────────────────────────────────────────────────────

#[async_trait]
impl LeaseStore for DynamoLeaseStore {
    async fn create(&self, resource_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error> {
        let now = Utc::now();
        let expires_at = if ttl_secs == LEASE_FOREVER {
            DateTime::<Utc>::MAX_UTC
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

        let mut req = self.client
            .put_item()
            .table_name(&self.table_name)
            .item("lease_id", AttributeValue::S(record.lease_id.clone()))
            .item("resource_id", AttributeValue::S(record.resource_id.clone()))
            .item(
                "granted_at",
                AttributeValue::S(record.granted_at.to_rfc3339()),
            )
            .item(
                "expires_at",
                AttributeValue::S(record.expires_at.to_rfc3339()),
            )
            .item(
                "ttl_seconds",
                AttributeValue::N(record.ttl_seconds.to_string()),
            );

        if let Some(epoch) = ttl_epoch(&record.expires_at, ttl_secs) {
            req = req.item("ttl", AttributeValue::N(epoch.to_string()));
        }

        req.send()
            .await
            .map_err(|e| Error::Storage(format!("put_item failed: {e}")))?;

        Ok(record)
    }

    async fn renew(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error> {
        // Fetch first so we can return the full updated record.
        let existing = self
            .get(lease_id)
            .await?
            .ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;

        let new_expires_at = if ttl_secs == LEASE_FOREVER {
            DateTime::<Utc>::MAX_UTC
        } else {
            Utc::now() + chrono::Duration::seconds(ttl_secs as i64)
        };

        let (update_expr, ttl_val) = match ttl_epoch(&new_expires_at, ttl_secs) {
            Some(epoch) => (
                "SET expires_at = :ea, ttl_seconds = :ts, #ttl_field = :ttl",
                Some(epoch),
            ),
            None => (
                "SET expires_at = :ea, ttl_seconds = :ts REMOVE #ttl_field",
                None,
            ),
        };

        let mut req = self.client
            .update_item()
            .table_name(&self.table_name)
            .key("lease_id", AttributeValue::S(lease_id.to_string()))
            .update_expression(update_expr)
            .expression_attribute_names("#ttl_field", "ttl")
            .expression_attribute_values(
                ":ea",
                AttributeValue::S(new_expires_at.to_rfc3339()),
            )
            .expression_attribute_values(":ts", AttributeValue::N(ttl_secs.to_string()))
            .condition_expression("attribute_exists(lease_id)");

        if let Some(epoch) = ttl_val {
            req = req.expression_attribute_values(":ttl", AttributeValue::N(epoch.to_string()));
        }

        req.send()
            .await
            .map_err(|e| Error::Storage(format!("update_item failed: {e}")))?;

        Ok(LeaseRecord {
            expires_at: new_expires_at,
            ttl_seconds: ttl_secs,
            ..existing
        })
    }

    async fn cancel(&self, lease_id: &str) -> Result<(), Error> {
        self.client
            .delete_item()
            .table_name(&self.table_name)
            .key("lease_id", AttributeValue::S(lease_id.to_string()))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;
        Ok(())
    }

    async fn get(&self, lease_id: &str) -> Result<Option<LeaseRecord>, Error> {
        let resp = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("lease_id", AttributeValue::S(lease_id.to_string()))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("get_item failed: {e}")))?;

        match resp.item {
            None => Ok(None),
            Some(item) => Ok(Some(record_from_item(&item)?)),
        }
    }

    async fn get_by_resource(&self, resource_id: &str) -> Result<Option<LeaseRecord>, Error> {
        let resp = self
            .client
            .query()
            .table_name(&self.table_name)
            .index_name(RESOURCE_GSI)
            .key_condition_expression("resource_id = :rid")
            .expression_attribute_values(":rid", AttributeValue::S(resource_id.to_string()))
            .limit(1)
            .send()
            .await
            .map_err(|e| Error::Storage(format!("query failed: {e}")))?;

        match resp.items.and_then(|items| items.into_iter().next()) {
            None => Ok(None),
            Some(item) => Ok(Some(record_from_item(&item)?)),
        }
    }

    async fn list_expired(&self) -> Result<Vec<LeaseRecord>, Error> {
        let now = Utc::now().to_rfc3339();

        let resp = self
            .client
            .scan()
            .table_name(&self.table_name)
            // Exclude FOREVER leases (ttl_seconds = 0) and only return records
            // whose expires_at is in the past.
            .filter_expression(
                "expires_at <= :now AND ttl_seconds <> :zero",
            )
            .expression_attribute_values(":now", AttributeValue::S(now))
            .expression_attribute_values(":zero", AttributeValue::N("0".to_string()))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("scan failed: {e}")))?;

        let records = resp
            .items
            .unwrap_or_default()
            .into_iter()
            .map(|item| record_from_item(&item))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(records)
    }

    async fn remove(&self, lease_id: &str) -> Result<(), Error> {
        self.cancel(lease_id).await
    }
}

// ── integration tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::make_dynamo_client;
    use aws_sdk_dynamodb::Client;

    /// Build a client pointing at MiniStack and create an isolated table for
    /// this test run. Returns (store, table_name) — caller must delete the table
    /// after the test.
    async fn setup() -> (DynamoLeaseStore, String, Client) {
        std::env::set_var("DYNAMODB_ENDPOINT", "http://localhost:4566");
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
        std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");

        let client = make_dynamo_client().await;
        let table_name = format!("coordin8_leases_test_{}", Uuid::new_v4());

        let store = DynamoLeaseStore::with_table(client.clone(), &table_name);
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

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn create_and_get() {
        let (store, table_name, client) = setup().await;

        let record = store.create("worker-1", 30).await.unwrap();
        assert_eq!(record.resource_id, "worker-1");
        assert!(!record.is_expired());

        let fetched = store.get(&record.lease_id).await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().resource_id, "worker-1");

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn cancel_removes_lease() {
        let (store, table_name, client) = setup().await;

        let record = store.create("worker-2", 30).await.unwrap();
        store.cancel(&record.lease_id).await.unwrap();
        assert!(store.get(&record.lease_id).await.unwrap().is_none());

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn expired_lease_shows_up_in_list() {
        let (store, table_name, client) = setup().await;

        let record = store.create("worker-3", 1).await.unwrap();
        // Wait for the 1s TTL to lapse.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        let expired = store.list_expired().await.unwrap();
        assert!(
            expired.iter().any(|r| r.lease_id == record.lease_id),
            "expected lease to appear in expired list"
        );

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn forever_lease_never_expires() {
        let (store, table_name, client) = setup().await;

        let record = store.create("worker-forever", 0).await.unwrap();
        assert_eq!(record.ttl_seconds, 0);
        assert!(!record.is_expired());

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let expired = store.list_expired().await.unwrap();
        assert!(
            !expired.iter().any(|r| r.lease_id == record.lease_id),
            "FOREVER lease must not appear in expired list"
        );

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn get_by_resource() {
        let (store, table_name, client) = setup().await;

        let record = store.create("my-service", 60).await.unwrap();
        let found = store.get_by_resource("my-service").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().lease_id, record.lease_id);

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn renew_updates_expiry() {
        let (store, table_name, client) = setup().await;

        let record = store.create("renewer", 10).await.unwrap();
        let original_expires = record.expires_at;

        // Renew with a longer TTL.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let renewed = store.renew(&record.lease_id, 3600).await.unwrap();
        assert!(
            renewed.expires_at > original_expires,
            "renewed expires_at should be later"
        );

        teardown(&client, &table_name).await;
    }
}
