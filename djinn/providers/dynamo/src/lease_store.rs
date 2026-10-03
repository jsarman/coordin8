use async_trait::async_trait;
use aws_sdk_dynamodb::{types::AttributeValue, Client};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use coordin8_core::{checked_expiry, Error, LeaseRecord, LeaseStore, LEASE_FOREVER};

use crate::table::{ensure_lease_table, RESOURCE_GSI};

pub struct DynamoLeaseStore {
    client: Client,
    table_name: String,
    /// Max items evaluated per scan page (DynamoDB `Limit`). `None` = server
    /// default (1MB pages). Exists so tests can force pagination cheaply.
    scan_page_limit: Option<i32>,
}

impl DynamoLeaseStore {
    /// Leasing is distributed — there's no single shared lease table, so
    /// there's no bare-default constructor either. Every caller names its
    /// own namespaced table (`coordin8-djinn/src/services.rs`'s
    /// `lease_store_from_env()` builds `coordin8_leases_{namespace}`).
    pub fn with_table(client: Client, table_name: impl Into<String>) -> Self {
        Self {
            client,
            table_name: table_name.into(),
            scan_page_limit: None,
        }
    }

    /// Cap the number of items evaluated per `list_expired` scan page.
    /// Intended for tests that need to exercise pagination.
    pub fn with_scan_page_limit(mut self, limit: i32) -> Self {
        self.scan_page_limit = Some(limit);
        self
    }

    /// Create the backing table if `COORDIN8_AUTO_CREATE_TABLES` is set to
    /// `"true"` or `"1"` (case-insensitive). Otherwise, assumes the table
    /// already exists (e.g. provisioned by CloudFormation).
    pub async fn init(&self) -> Result<(), Error> {
        if crate::auto_create_enabled() {
            ensure_lease_table(&self.client, &self.table_name)
                .await
                .map_err(Error::Storage)
        } else {
            tracing::info!(
                table = %self.table_name,
                "COORDIN8_AUTO_CREATE_TABLES not set — skipping table creation"
            );
            Ok(())
        }
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

/// Expiry for a new grant/renewal; FOREVER maps to the far-future sentinel and
/// unrepresentable TTLs are an error rather than a panic.
fn expiry_for(from: DateTime<Utc>, ttl_secs: u64) -> Result<DateTime<Utc>, Error> {
    if ttl_secs == LEASE_FOREVER {
        return Ok(DateTime::<Utc>::MAX_UTC);
    }
    checked_expiry(from, ttl_secs)
        .ok_or_else(|| Error::Storage(format!("lease ttl out of range: {ttl_secs}s")))
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
        let expires_at = expiry_for(now, ttl_secs)?;

        let record = LeaseRecord {
            lease_id: Uuid::new_v4().to_string(),
            resource_id: resource_id.to_string(),
            granted_at: now,
            expires_at,
            ttl_seconds: ttl_secs,
        };

        let mut req = self
            .client
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

        let new_expires_at = expiry_for(Utc::now(), ttl_secs)?;

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

        let mut req = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("lease_id", AttributeValue::S(lease_id.to_string()))
            .update_expression(update_expr)
            .expression_attribute_names("#ttl_field", "ttl")
            .expression_attribute_values(":ea", AttributeValue::S(new_expires_at.to_rfc3339()))
            .expression_attribute_values(":ts", AttributeValue::N(ttl_secs.to_string()))
            .condition_expression("attribute_exists(lease_id)");

        if let Some(epoch) = ttl_val {
            req = req.expression_attribute_values(":ttl", AttributeValue::N(epoch.to_string()));
        }

        req.send().await.map_err(|e| {
            if e.as_service_error()
                .is_some_and(|se| se.is_conditional_check_failed_exception())
            {
                Error::LeaseNotFound(lease_id.to_string())
            } else {
                Error::Storage(format!("update_item failed: {e}"))
            }
        })?;

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
        let now = Utc::now();
        let mut records = Vec::new();
        let mut last_key: Option<std::collections::HashMap<String, AttributeValue>> = None;

        loop {
            // Pre-filter on the numeric `ttl` epoch. FOREVER leases have no
            // `ttl` attribute, so they are excluded. The epoch is truncated to
            // seconds, so this is a superset; `is_expired()` below is the
            // final authority.
            let mut req = self
                .client
                .scan()
                .table_name(&self.table_name)
                .filter_expression("attribute_exists(#ttl_field) AND #ttl_field <= :now")
                .expression_attribute_names("#ttl_field", "ttl")
                .expression_attribute_values(
                    ":now",
                    AttributeValue::N(now.timestamp().to_string()),
                );
            if let Some(limit) = self.scan_page_limit {
                req = req.limit(limit);
            }
            if let Some(ref lek) = last_key {
                req = req.set_exclusive_start_key(Some(lek.clone()));
            }

            let resp = req
                .send()
                .await
                .map_err(|e| Error::Storage(format!("scan failed: {e}")))?;

            for item in resp.items.unwrap_or_default() {
                let record = record_from_item(&item)?;
                if record.is_expired() {
                    records.push(record);
                }
            }

            last_key = resp.last_evaluated_key;
            if last_key.is_none() {
                break;
            }
        }

        Ok(records)
    }

    async fn remove_if_unchanged(&self, record: &LeaseRecord) -> Result<bool, Error> {
        let result = self
            .client
            .delete_item()
            .table_name(&self.table_name)
            .key("lease_id", AttributeValue::S(record.lease_id.clone()))
            .condition_expression("expires_at = :listed")
            .expression_attribute_values(
                ":listed",
                AttributeValue::S(record.expires_at.to_rfc3339()),
            )
            .send()
            .await;

        match result {
            Ok(_) => Ok(true),
            Err(e) => {
                let is_condition_check = e
                    .as_service_error()
                    .is_some_and(|se| se.is_conditional_check_failed_exception());
                if is_condition_check {
                    Ok(false)
                } else {
                    Err(Error::Storage(format!("delete_item failed: {e}")))
                }
            }
        }
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
        std::env::set_var("COORDIN8_AUTO_CREATE_TABLES", "true");

        let client = make_dynamo_client().await;
        let table_name = format!("coordin8_leases_test_{}", Uuid::new_v4());

        let store = DynamoLeaseStore::with_table(client.clone(), &table_name);
        store.init().await.expect("table creation failed");

        (store, table_name, client)
    }

    async fn teardown(client: &Client, table_name: &str) {
        let _ = client.delete_table().table_name(table_name).send().await;
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

        let record = store
            .create("worker-forever", coordin8_core::LEASE_FOREVER)
            .await
            .unwrap();
        assert_eq!(record.ttl_seconds, coordin8_core::LEASE_FOREVER);
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

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn forever_lease_not_listed_even_when_others_expired() {
        let (store, table_name, client) = setup().await;

        let forever = store
            .create("f", coordin8_core::LEASE_FOREVER)
            .await
            .unwrap();
        let short = store.create("s", 1).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        let expired = store.list_expired().await.unwrap();
        assert!(expired.iter().any(|r| r.lease_id == short.lease_id));
        assert!(!expired.iter().any(|r| r.lease_id == forever.lease_id));

        teardown(&client, &table_name).await;
    }

    /// Pagination: a scan page limit of 2 with 7 expired leases (plus a live
    /// and a FOREVER lease interleaved) forces multiple pages, some of which
    /// contain no matching items.
    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn list_expired_paginates() {
        let (store, table_name, client) = setup().await;
        let store = store.with_scan_page_limit(2);

        let mut expired_ids = Vec::new();
        for i in 0..7 {
            expired_ids.push(store.create(&format!("e-{i}"), 1).await.unwrap().lease_id);
        }
        let live = store.create("live", 3600).await.unwrap();
        let forever = store
            .create("forever", coordin8_core::LEASE_FOREVER)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        let listed: Vec<String> = store
            .list_expired()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.lease_id)
            .collect();
        assert_eq!(listed.len(), 7, "listed: {listed:?}");
        for id in &expired_ids {
            assert!(listed.contains(id));
        }
        assert!(!listed.contains(&live.lease_id));
        assert!(!listed.contains(&forever.lease_id));

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn remove_if_unchanged_is_conditional() {
        let (store, table_name, client) = setup().await;

        let record = store.create("cond", 1).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let expired = store.list_expired().await.unwrap();
        let listed = expired
            .iter()
            .find(|r| r.lease_id == record.lease_id)
            .unwrap()
            .clone();

        // Renewed after listing: must not be removed.
        store.renew(&record.lease_id, 60).await.unwrap();
        assert!(!store.remove_if_unchanged(&listed).await.unwrap());
        assert!(store.get(&record.lease_id).await.unwrap().is_some());

        // Unchanged: removed. Then already gone: false.
        let current = store.get(&record.lease_id).await.unwrap().unwrap();
        assert!(store.remove_if_unchanged(&current).await.unwrap());
        assert!(store.get(&record.lease_id).await.unwrap().is_none());
        assert!(!store.remove_if_unchanged(&current).await.unwrap());

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn huge_ttl_does_not_panic() {
        let (store, table_name, client) = setup().await;
        assert!(store.create("huge", u64::MAX - 1).await.is_err());
        teardown(&client, &table_name).await;
    }
}
