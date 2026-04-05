use std::collections::HashMap;

use async_trait::async_trait;
use aws_sdk_dynamodb::{Client, types::AttributeValue};
use chrono::{DateTime, Utc};

use coordin8_core::{Error, SpaceEventKind, SpaceStore, SpaceWatchRecord, TupleRecord};
use coordin8_registry::matcher::{matches, parse_template};

use crate::table::{
    ensure_space_table, ensure_space_txn_taken_table, ensure_space_uncommitted_table,
    ensure_space_watches_table, SPACE_LEASE_GSI, SPACE_TABLE, SPACE_TXN_TAKEN_TABLE,
    SPACE_UNCOMMITTED_TABLE, SPACE_WATCHES_LEASE_GSI, SPACE_WATCHES_TABLE,
};

pub struct DynamoSpaceStore {
    client: Client,
    tuple_table: String,
    uncommitted_table: String,
    txn_taken_table: String,
    watches_table: String,
}

impl DynamoSpaceStore {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            tuple_table: SPACE_TABLE.to_string(),
            uncommitted_table: SPACE_UNCOMMITTED_TABLE.to_string(),
            txn_taken_table: SPACE_TXN_TAKEN_TABLE.to_string(),
            watches_table: SPACE_WATCHES_TABLE.to_string(),
        }
    }

    pub fn with_tables(
        client: Client,
        tuple_table: impl Into<String>,
        uncommitted_table: impl Into<String>,
        txn_taken_table: impl Into<String>,
        watches_table: impl Into<String>,
    ) -> Self {
        Self {
            client,
            tuple_table: tuple_table.into(),
            uncommitted_table: uncommitted_table.into(),
            txn_taken_table: txn_taken_table.into(),
            watches_table: watches_table.into(),
        }
    }

    pub async fn init(&self) -> Result<(), Error> {
        ensure_space_table(&self.client, &self.tuple_table)
            .await
            .map_err(Error::Storage)?;
        ensure_space_uncommitted_table(&self.client, &self.uncommitted_table)
            .await
            .map_err(Error::Storage)?;
        ensure_space_txn_taken_table(&self.client, &self.txn_taken_table)
            .await
            .map_err(Error::Storage)?;
        ensure_space_watches_table(&self.client, &self.watches_table)
            .await
            .map_err(Error::Storage)?;
        Ok(())
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn tuple_to_item(record: &TupleRecord) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::new();
    item.insert("tuple_id".to_string(), AttributeValue::S(record.tuple_id.clone()));
    item.insert("lease_id".to_string(), AttributeValue::S(record.lease_id.clone()));
    item.insert("written_by".to_string(), AttributeValue::S(record.written_by.clone()));
    item.insert(
        "written_at".to_string(),
        AttributeValue::S(record.written_at.to_rfc3339()),
    );

    let attrs_map: HashMap<String, AttributeValue> = record
        .attrs
        .iter()
        .map(|(k, v)| (k.clone(), AttributeValue::S(v.clone())))
        .collect();
    item.insert("attrs".to_string(), AttributeValue::M(attrs_map));

    if !record.payload.is_empty() {
        item.insert(
            "payload".to_string(),
            AttributeValue::B(aws_sdk_dynamodb::primitives::Blob::new(record.payload.clone())),
        );
    }

    if let Some(ref input_id) = record.input_tuple_id {
        item.insert("input_tuple_id".to_string(), AttributeValue::S(input_id.clone()));
    }

    item
}

fn tuple_from_item(item: &HashMap<String, AttributeValue>) -> Result<TupleRecord, Error> {
    let tuple_id = item
        .get("tuple_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing tuple_id".into()))?
        .clone();

    let lease_id = item
        .get("lease_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing lease_id".into()))?
        .clone();

    let written_by = item
        .get("written_by")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing written_by".into()))?
        .clone();

    let written_at: DateTime<Utc> = item
        .get("written_at")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing written_at".into()))
        .and_then(|s| {
            s.parse::<DateTime<Utc>>()
                .map_err(|e| Error::Storage(format!("bad written_at: {e}")))
        })?;

    let attrs = match item.get("attrs") {
        Some(AttributeValue::M(map)) => map
            .iter()
            .map(|(k, v)| {
                v.as_s()
                    .map(|s| (k.clone(), s.clone()))
                    .map_err(|_| Error::Storage(format!("attrs value for '{k}' is not a string")))
            })
            .collect::<Result<HashMap<String, String>, Error>>()?,
        _ => HashMap::new(),
    };

    let payload = match item.get("payload") {
        Some(AttributeValue::B(blob)) => blob.as_ref().to_vec(),
        _ => vec![],
    };

    let input_tuple_id = item
        .get("input_tuple_id")
        .and_then(|v| v.as_s().ok())
        .cloned();

    Ok(TupleRecord {
        tuple_id,
        attrs,
        payload,
        lease_id,
        written_by,
        written_at,
        input_tuple_id,
    })
}

fn watch_from_item(item: &HashMap<String, AttributeValue>) -> Result<SpaceWatchRecord, Error> {
    let watch_id = item
        .get("watch_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing watch_id".into()))?
        .clone();

    let lease_id = item
        .get("lease_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing lease_id".into()))?
        .clone();

    let on = match item
        .get("on")
        .and_then(|v| v.as_s().ok())
        .map(|s| s.as_str())
    {
        Some("Appearance") => SpaceEventKind::Appearance,
        Some("Expiration") => SpaceEventKind::Expiration,
        other => {
            return Err(Error::Storage(format!(
                "unknown SpaceEventKind: {:?}",
                other
            )))
        }
    };

    let template = match item.get("template") {
        Some(AttributeValue::M(map)) => map
            .iter()
            .map(|(k, v)| {
                v.as_s()
                    .map(|s| (k.clone(), s.clone()))
                    .map_err(|_| Error::Storage(format!("template value for '{k}' is not a string")))
            })
            .collect::<Result<HashMap<String, String>, Error>>()?,
        _ => HashMap::new(),
    };

    let handback = match item.get("handback") {
        Some(AttributeValue::B(blob)) => blob.as_ref().to_vec(),
        _ => vec![],
    };

    Ok(SpaceWatchRecord {
        watch_id,
        template,
        on,
        lease_id,
        handback,
    })
}

/// Scan all items from a DynamoDB table, handling pagination.
async fn scan_all(
    client: &Client,
    table_name: &str,
) -> Result<Vec<HashMap<String, AttributeValue>>, Error> {
    let mut items = Vec::new();
    let mut last_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut req = client.scan().table_name(table_name);
        if let Some(ref lek) = last_key {
            req = req.set_exclusive_start_key(Some(lek.clone()));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Error::Storage(format!("scan failed: {e}")))?;

        items.extend(resp.items.unwrap_or_default());

        last_key = resp.last_evaluated_key;
        if last_key.is_none() {
            break;
        }
    }

    Ok(items)
}

/// Query all items by partition key from a DynamoDB table.
async fn query_by_pk(
    client: &Client,
    table_name: &str,
    pk_name: &str,
    pk_value: &str,
) -> Result<Vec<HashMap<String, AttributeValue>>, Error> {
    let mut items = Vec::new();
    let mut last_key: Option<HashMap<String, AttributeValue>> = None;

    loop {
        let mut req = client
            .query()
            .table_name(table_name)
            .key_condition_expression(format!("{pk_name} = :pk"))
            .expression_attribute_values(":pk", AttributeValue::S(pk_value.to_string()));
        if let Some(ref lek) = last_key {
            req = req.set_exclusive_start_key(Some(lek.clone()));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Error::Storage(format!("query failed: {e}")))?;

        items.extend(resp.items.unwrap_or_default());

        last_key = resp.last_evaluated_key;
        if last_key.is_none() {
            break;
        }
    }

    Ok(items)
}

// ── trait implementation ──────────────────────────────────────────────────────

#[async_trait]
impl SpaceStore for DynamoSpaceStore {
    async fn insert(&self, record: TupleRecord) -> Result<(), Error> {
        let item = tuple_to_item(&record);
        self.client
            .put_item()
            .table_name(&self.tuple_table)
            .set_item(Some(item))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("put_item failed: {e}")))?;
        Ok(())
    }

    async fn insert_uncommitted(&self, txn_id: &str, record: TupleRecord) -> Result<(), Error> {
        let mut item = tuple_to_item(&record);
        item.insert("txn_id".to_string(), AttributeValue::S(txn_id.to_string()));

        self.client
            .put_item()
            .table_name(&self.uncommitted_table)
            .set_item(Some(item))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("put_item failed: {e}")))?;
        Ok(())
    }

    async fn get(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error> {
        let resp = self
            .client
            .get_item()
            .table_name(&self.tuple_table)
            .key("tuple_id", AttributeValue::S(tuple_id.to_string()))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("get_item failed: {e}")))?;

        match resp.item {
            None => Ok(None),
            Some(item) => Ok(Some(tuple_from_item(&item)?)),
        }
    }

    async fn remove(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error> {
        let resp = self
            .client
            .delete_item()
            .table_name(&self.tuple_table)
            .key("tuple_id", AttributeValue::S(tuple_id.to_string()))
            .return_values(aws_sdk_dynamodb::types::ReturnValue::AllOld)
            .send()
            .await
            .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;

        match resp.attributes {
            None => Ok(None),
            Some(item) => Ok(Some(tuple_from_item(&item)?)),
        }
    }

    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<TupleRecord>, Error> {
        // Query the GSI to find the tuple_id for this lease
        let resp = self
            .client
            .query()
            .table_name(&self.tuple_table)
            .index_name(SPACE_LEASE_GSI)
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

        let tuple_id = item
            .get("tuple_id")
            .and_then(|v| v.as_s().ok())
            .ok_or_else(|| Error::Storage("GSI item missing tuple_id".into()))?
            .clone();

        self.remove(&tuple_id).await
    }

    async fn find_match(
        &self,
        template: &HashMap<String, String>,
        txn_id: Option<&str>,
    ) -> Result<Option<TupleRecord>, Error> {
        let ops = parse_template(template);

        // Search committed tuples
        let items = scan_all(&self.client, &self.tuple_table).await?;
        for item in &items {
            let record = tuple_from_item(item)?;
            if matches(&ops, &record.attrs) {
                return Ok(Some(record));
            }
        }

        // Search uncommitted buffer for this transaction
        if let Some(tid) = txn_id {
            let uncommitted = query_by_pk(&self.client, &self.uncommitted_table, "txn_id", tid).await?;
            for item in &uncommitted {
                let record = tuple_from_item(item)?;
                if matches(&ops, &record.attrs) {
                    return Ok(Some(record));
                }
            }
        }

        Ok(None)
    }

    async fn take_match(
        &self,
        template: &HashMap<String, String>,
        txn_id: Option<&str>,
    ) -> Result<Option<TupleRecord>, Error> {
        let ops = parse_template(template);

        // If txn_id provided, search uncommitted buffer first
        if let Some(tid) = txn_id {
            let uncommitted =
                query_by_pk(&self.client, &self.uncommitted_table, "txn_id", tid).await?;
            for item in &uncommitted {
                let record = tuple_from_item(item)?;
                if matches(&ops, &record.attrs) {
                    // Remove from uncommitted
                    self.client
                        .delete_item()
                        .table_name(&self.uncommitted_table)
                        .key("txn_id", AttributeValue::S(tid.to_string()))
                        .key("tuple_id", AttributeValue::S(record.tuple_id.clone()))
                        .send()
                        .await
                        .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;
                    return Ok(Some(record));
                }
            }
        }

        // Search committed tuples — use conditional delete for atomicity
        let items = scan_all(&self.client, &self.tuple_table).await?;
        for item in &items {
            let record = tuple_from_item(item)?;
            if matches(&ops, &record.attrs) {
                // Atomic take: conditional delete (only succeeds if item still exists)
                let result = self
                    .client
                    .delete_item()
                    .table_name(&self.tuple_table)
                    .key("tuple_id", AttributeValue::S(record.tuple_id.clone()))
                    .condition_expression("attribute_exists(tuple_id)")
                    .return_values(aws_sdk_dynamodb::types::ReturnValue::AllOld)
                    .send()
                    .await;

                match result {
                    Ok(resp) => {
                        if let Some(deleted_item) = resp.attributes {
                            let taken = tuple_from_item(&deleted_item)?;

                            // Track taken tuple for restore-on-abort
                            if let Some(tid) = txn_id {
                                let mut taken_item = tuple_to_item(&taken);
                                taken_item.insert(
                                    "txn_id".to_string(),
                                    AttributeValue::S(tid.to_string()),
                                );
                                self.client
                                    .put_item()
                                    .table_name(&self.txn_taken_table)
                                    .set_item(Some(taken_item))
                                    .send()
                                    .await
                                    .map_err(|e| {
                                        Error::Storage(format!("put_item (txn_taken) failed: {e}"))
                                    })?;
                            }

                            return Ok(Some(taken));
                        }
                    }
                    Err(e) => {
                        // Conditional check failed — someone else took it, try next match
                        let is_condition_check = e
                            .as_service_error()
                            .map_or(false, |se| se.is_conditional_check_failed_exception());
                        if is_condition_check {
                            continue;
                        }
                        return Err(Error::Storage(format!("delete_item failed: {e}")));
                    }
                }
            }
        }

        Ok(None)
    }

    async fn find_all_matches(
        &self,
        template: &HashMap<String, String>,
        txn_id: Option<&str>,
    ) -> Result<Vec<TupleRecord>, Error> {
        let ops = parse_template(template);
        let mut results = Vec::new();

        // Committed tuples
        let items = scan_all(&self.client, &self.tuple_table).await?;
        for item in &items {
            let record = tuple_from_item(item)?;
            if ops.is_empty() || matches(&ops, &record.attrs) {
                results.push(record);
            }
        }

        // Uncommitted tuples for this transaction
        if let Some(tid) = txn_id {
            let uncommitted =
                query_by_pk(&self.client, &self.uncommitted_table, "txn_id", tid).await?;
            for item in &uncommitted {
                let record = tuple_from_item(item)?;
                if ops.is_empty() || matches(&ops, &record.attrs) {
                    results.push(record);
                }
            }
        }

        Ok(results)
    }

    async fn commit_txn(&self, txn_id: &str) -> Result<Vec<TupleRecord>, Error> {
        // Flush uncommitted writes to the visible store
        let uncommitted_items =
            query_by_pk(&self.client, &self.uncommitted_table, "txn_id", txn_id).await?;

        let mut flushed = Vec::new();
        for item in &uncommitted_items {
            let record = tuple_from_item(item)?;
            self.insert(record.clone()).await?;
            flushed.push(record);

            // Remove from uncommitted
            self.client
                .delete_item()
                .table_name(&self.uncommitted_table)
                .key("txn_id", AttributeValue::S(txn_id.to_string()))
                .key("tuple_id", AttributeValue::S(item.get("tuple_id").unwrap().as_s().unwrap().clone()))
                .send()
                .await
                .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;
        }

        // Finalize takes — clean up tracking buffer (they stay removed)
        let taken_items =
            query_by_pk(&self.client, &self.txn_taken_table, "txn_id", txn_id).await?;
        for item in &taken_items {
            self.client
                .delete_item()
                .table_name(&self.txn_taken_table)
                .key("txn_id", AttributeValue::S(txn_id.to_string()))
                .key("tuple_id", AttributeValue::S(item.get("tuple_id").unwrap().as_s().unwrap().clone()))
                .send()
                .await
                .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;
        }

        Ok(flushed)
    }

    async fn abort_txn(&self, txn_id: &str) -> Result<(Vec<TupleRecord>, Vec<TupleRecord>), Error> {
        // Discard uncommitted writes
        let uncommitted_items =
            query_by_pk(&self.client, &self.uncommitted_table, "txn_id", txn_id).await?;

        let mut discarded = Vec::new();
        for item in &uncommitted_items {
            let record = tuple_from_item(item)?;
            discarded.push(record);

            self.client
                .delete_item()
                .table_name(&self.uncommitted_table)
                .key("txn_id", AttributeValue::S(txn_id.to_string()))
                .key("tuple_id", AttributeValue::S(item.get("tuple_id").unwrap().as_s().unwrap().clone()))
                .send()
                .await
                .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;
        }

        // Restore taken tuples back to committed store
        let taken_items =
            query_by_pk(&self.client, &self.txn_taken_table, "txn_id", txn_id).await?;

        let mut restored = Vec::new();
        for item in &taken_items {
            let record = tuple_from_item(item)?;
            self.insert(record.clone()).await?;
            restored.push(record);

            self.client
                .delete_item()
                .table_name(&self.txn_taken_table)
                .key("txn_id", AttributeValue::S(txn_id.to_string()))
                .key("tuple_id", AttributeValue::S(item.get("tuple_id").unwrap().as_s().unwrap().clone()))
                .send()
                .await
                .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;
        }

        Ok((discarded, restored))
    }

    async fn has_txn(&self, txn_id: &str) -> Result<bool, Error> {
        // Check uncommitted
        let uncommitted =
            query_by_pk(&self.client, &self.uncommitted_table, "txn_id", txn_id).await?;
        if !uncommitted.is_empty() {
            return Ok(true);
        }

        // Check txn_taken
        let taken = query_by_pk(&self.client, &self.txn_taken_table, "txn_id", txn_id).await?;
        Ok(!taken.is_empty())
    }

    async fn list_all(&self) -> Result<Vec<TupleRecord>, Error> {
        let items = scan_all(&self.client, &self.tuple_table).await?;
        items.iter().map(tuple_from_item).collect()
    }

    async fn create_watch(&self, watch: SpaceWatchRecord) -> Result<(), Error> {
        let template_map: HashMap<String, AttributeValue> = watch
            .template
            .iter()
            .map(|(k, v)| (k.clone(), AttributeValue::S(v.clone())))
            .collect();

        let on_str = match watch.on {
            SpaceEventKind::Appearance => "Appearance",
            SpaceEventKind::Expiration => "Expiration",
        };

        let mut req = self
            .client
            .put_item()
            .table_name(&self.watches_table)
            .item("watch_id", AttributeValue::S(watch.watch_id))
            .item("lease_id", AttributeValue::S(watch.lease_id))
            .item("on", AttributeValue::S(on_str.to_string()))
            .item("template", AttributeValue::M(template_map));

        if !watch.handback.is_empty() {
            req = req.item(
                "handback",
                AttributeValue::B(aws_sdk_dynamodb::primitives::Blob::new(watch.handback)),
            );
        }

        req.send()
            .await
            .map_err(|e| Error::Storage(format!("put_item failed: {e}")))?;
        Ok(())
    }

    async fn remove_watch(&self, watch_id: &str) -> Result<(), Error> {
        self.client
            .delete_item()
            .table_name(&self.watches_table)
            .key("watch_id", AttributeValue::S(watch_id.to_string()))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;
        Ok(())
    }

    async fn remove_watch_by_lease(
        &self,
        lease_id: &str,
    ) -> Result<Option<SpaceWatchRecord>, Error> {
        let resp = self
            .client
            .query()
            .table_name(&self.watches_table)
            .index_name(SPACE_WATCHES_LEASE_GSI)
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

        let watch = watch_from_item(&item)?;
        let watch_id = watch.watch_id.clone();

        self.client
            .delete_item()
            .table_name(&self.watches_table)
            .key("watch_id", AttributeValue::S(watch_id))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;

        Ok(Some(watch))
    }

    async fn list_watches(&self) -> Result<Vec<SpaceWatchRecord>, Error> {
        let items = scan_all(&self.client, &self.watches_table).await?;
        items.iter().map(watch_from_item).collect()
    }
}

// ── integration tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::make_dynamo_client;
    use aws_sdk_dynamodb::Client;
    use uuid::Uuid;

    async fn setup() -> (DynamoSpaceStore, Vec<String>, Client) {
        std::env::set_var("DYNAMODB_ENDPOINT", "http://localhost:4566");
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
        std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");

        let client = make_dynamo_client().await;
        let suffix = Uuid::new_v4();
        let tables = vec![
            format!("coordin8_space_test_{suffix}"),
            format!("coordin8_space_uncommitted_test_{suffix}"),
            format!("coordin8_space_txn_taken_test_{suffix}"),
            format!("coordin8_space_watches_test_{suffix}"),
        ];

        let store = DynamoSpaceStore::with_tables(
            client.clone(),
            &tables[0],
            &tables[1],
            &tables[2],
            &tables[3],
        );
        store.init().await.expect("table creation failed");

        (store, tables, client)
    }

    async fn teardown(client: &Client, tables: &[String]) {
        for table in tables {
            let _ = client.delete_table().table_name(table).send().await;
        }
    }

    fn make_tuple(tuple_id: &str, lease_id: &str) -> TupleRecord {
        let mut attrs = HashMap::new();
        attrs.insert("type".to_string(), "order".to_string());
        attrs.insert("symbol".to_string(), "AAPL".to_string());

        TupleRecord {
            tuple_id: tuple_id.to_string(),
            attrs,
            payload: vec![1, 2, 3],
            lease_id: lease_id.to_string(),
            written_by: "test-client".to_string(),
            written_at: Utc::now(),
            input_tuple_id: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn insert_and_get() {
        let (store, tables, client) = setup().await;

        let tuple = make_tuple("t-1", "lease-1");
        store.insert(tuple).await.unwrap();

        let fetched = store.get("t-1").await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.tuple_id, "t-1");
        assert_eq!(fetched.lease_id, "lease-1");
        assert_eq!(fetched.attrs.get("symbol").unwrap(), "AAPL");
        assert_eq!(fetched.payload, vec![1, 2, 3]);

        teardown(&client, &tables).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn remove_returns_old() {
        let (store, tables, client) = setup().await;

        store.insert(make_tuple("t-rm", "lease-rm")).await.unwrap();
        let removed = store.remove("t-rm").await.unwrap();
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().tuple_id, "t-rm");
        assert!(store.get("t-rm").await.unwrap().is_none());

        teardown(&client, &tables).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn remove_by_lease() {
        let (store, tables, client) = setup().await;

        store.insert(make_tuple("t-lease", "lease-target")).await.unwrap();
        let removed = store.remove_by_lease("lease-target").await.unwrap();
        assert!(removed.is_some());
        assert!(store.get("t-lease").await.unwrap().is_none());

        teardown(&client, &tables).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn find_match() {
        let (store, tables, client) = setup().await;

        store.insert(make_tuple("t-find", "lease-f")).await.unwrap();

        let mut template = HashMap::new();
        template.insert("symbol".to_string(), "AAPL".to_string());
        let found = store.find_match(&template, None).await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().tuple_id, "t-find");

        let mut no_match = HashMap::new();
        no_match.insert("symbol".to_string(), "GOOG".to_string());
        let not_found = store.find_match(&no_match, None).await.unwrap();
        assert!(not_found.is_none());

        teardown(&client, &tables).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn take_match_atomic() {
        let (store, tables, client) = setup().await;

        store.insert(make_tuple("t-take", "lease-t")).await.unwrap();

        let mut template = HashMap::new();
        template.insert("type".to_string(), "order".to_string());

        let taken = store.take_match(&template, None).await.unwrap();
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().tuple_id, "t-take");

        // Should be gone now
        assert!(store.get("t-take").await.unwrap().is_none());

        // Second take should find nothing
        let taken2 = store.take_match(&template, None).await.unwrap();
        assert!(taken2.is_none());

        teardown(&client, &tables).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn txn_uncommitted_visible_within_txn() {
        let (store, tables, client) = setup().await;

        let tuple = make_tuple("t-uncommitted", "lease-uc");
        store.insert_uncommitted("txn-1", tuple).await.unwrap();

        // Not visible without txn context
        let mut template = HashMap::new();
        template.insert("symbol".to_string(), "AAPL".to_string());
        let without_txn = store.find_match(&template, None).await.unwrap();
        assert!(without_txn.is_none());

        // Visible with txn context
        let with_txn = store.find_match(&template, Some("txn-1")).await.unwrap();
        assert!(with_txn.is_some());

        teardown(&client, &tables).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn commit_flushes_uncommitted() {
        let (store, tables, client) = setup().await;

        store
            .insert_uncommitted("txn-commit", make_tuple("t-c1", "lease-c1"))
            .await
            .unwrap();

        let flushed = store.commit_txn("txn-commit").await.unwrap();
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].tuple_id, "t-c1");

        // Now visible without txn context
        let fetched = store.get("t-c1").await.unwrap();
        assert!(fetched.is_some());

        teardown(&client, &tables).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn abort_discards_uncommitted_restores_taken() {
        let (store, tables, client) = setup().await;

        // Insert a committed tuple
        store.insert(make_tuple("t-taken", "lease-taken")).await.unwrap();

        // Insert an uncommitted tuple with different attrs so it won't match the take
        let mut uc_tuple = make_tuple("t-uc", "lease-uc");
        uc_tuple.attrs.insert("type".to_string(), "receipt".to_string());
        store
            .insert_uncommitted("txn-abort", uc_tuple)
            .await
            .unwrap();

        // Take the committed tuple under the txn (matches "order", not "receipt")
        let mut template = HashMap::new();
        template.insert("type".to_string(), "order".to_string());
        let taken = store.take_match(&template, Some("txn-abort")).await.unwrap();
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().tuple_id, "t-taken");

        // Abort — uncommitted should be discarded, taken should be restored
        let (discarded, restored) = store.abort_txn("txn-abort").await.unwrap();
        assert_eq!(discarded.len(), 1);
        assert_eq!(discarded[0].tuple_id, "t-uc");
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].tuple_id, "t-taken");

        // Taken tuple is back
        let fetched = store.get("t-taken").await.unwrap();
        assert!(fetched.is_some());

        // Uncommitted tuple is gone
        let fetched_uc = store.get("t-uc").await.unwrap();
        assert!(fetched_uc.is_none());

        teardown(&client, &tables).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn watches_crud() {
        let (store, tables, client) = setup().await;

        let mut template = HashMap::new();
        template.insert("type".to_string(), "order".to_string());

        let watch = SpaceWatchRecord {
            watch_id: "w-1".to_string(),
            template,
            on: SpaceEventKind::Appearance,
            lease_id: "lease-w".to_string(),
            handback: vec![99],
        };

        store.create_watch(watch).await.unwrap();

        let watches = store.list_watches().await.unwrap();
        assert_eq!(watches.len(), 1);
        assert_eq!(watches[0].watch_id, "w-1");
        assert_eq!(watches[0].on, SpaceEventKind::Appearance);
        assert_eq!(watches[0].handback, vec![99]);

        // Remove by lease
        let removed = store.remove_watch_by_lease("lease-w").await.unwrap();
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().watch_id, "w-1");

        let watches = store.list_watches().await.unwrap();
        assert!(watches.is_empty());

        teardown(&client, &tables).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn has_txn() {
        let (store, tables, client) = setup().await;

        assert!(!store.has_txn("txn-check").await.unwrap());

        store
            .insert_uncommitted("txn-check", make_tuple("t-htx", "lease-htx"))
            .await
            .unwrap();

        assert!(store.has_txn("txn-check").await.unwrap());

        teardown(&client, &tables).await;
    }
}
