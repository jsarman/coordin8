use std::collections::HashMap;

use async_trait::async_trait;
use aws_sdk_dynamodb::{types::AttributeValue, Client};
use chrono::{DateTime, Utc};

use coordin8_core::{Error, ParticipantRecord, TransactionRecord, TransactionState, TxnStore};

use crate::table::{ensure_txn_table, TXN_TABLE};

pub struct DynamoTxnStore {
    client: Client,
    table_name: String,
}

impl DynamoTxnStore {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            table_name: TXN_TABLE.to_string(),
        }
    }

    pub fn with_table(client: Client, table_name: impl Into<String>) -> Self {
        Self {
            client,
            table_name: table_name.into(),
        }
    }

    pub async fn init(&self) -> Result<(), Error> {
        ensure_txn_table(&self.client, &self.table_name)
            .await
            .map_err(Error::Storage)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn state_to_str(state: &TransactionState) -> &'static str {
    match state {
        TransactionState::Active => "Active",
        TransactionState::Voting => "Voting",
        TransactionState::Prepared => "Prepared",
        TransactionState::Committed => "Committed",
        TransactionState::Aborted => "Aborted",
    }
}

fn state_from_str(s: &str) -> Result<TransactionState, Error> {
    match s {
        "Active" => Ok(TransactionState::Active),
        "Voting" => Ok(TransactionState::Voting),
        "Prepared" => Ok(TransactionState::Prepared),
        "Committed" => Ok(TransactionState::Committed),
        "Aborted" => Ok(TransactionState::Aborted),
        other => Err(Error::Storage(format!(
            "unknown transaction state: {other}"
        ))),
    }
}

fn participant_to_av(p: &ParticipantRecord) -> AttributeValue {
    let mut map = HashMap::new();
    map.insert(
        "endpoint".to_string(),
        AttributeValue::S(p.endpoint.clone()),
    );
    map.insert(
        "crash_count".to_string(),
        AttributeValue::N(p.crash_count.to_string()),
    );
    AttributeValue::M(map)
}

fn participant_from_av(av: &AttributeValue) -> Result<ParticipantRecord, Error> {
    let map = av
        .as_m()
        .map_err(|_| Error::Storage("participant is not a map".into()))?;

    let endpoint = map
        .get("endpoint")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing participant endpoint".into()))?
        .clone();

    let crash_count = map
        .get("crash_count")
        .and_then(|v| v.as_n().ok())
        .ok_or_else(|| Error::Storage("missing participant crash_count".into()))
        .and_then(|s| {
            s.parse::<u64>()
                .map_err(|e| Error::Storage(format!("bad crash_count: {e}")))
        })?;

    Ok(ParticipantRecord {
        endpoint,
        crash_count,
    })
}

fn record_from_item(item: &HashMap<String, AttributeValue>) -> Result<TransactionRecord, Error> {
    let txn_id = item
        .get("txn_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing txn_id".into()))?
        .clone();

    let lease_id = item
        .get("lease_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing lease_id".into()))?
        .clone();

    let state = item
        .get("state")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing state".into()))
        .and_then(|s| state_from_str(s))?;

    let begun_at: DateTime<Utc> = item
        .get("begun_at")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing begun_at".into()))
        .and_then(|s| {
            s.parse::<DateTime<Utc>>()
                .map_err(|e| Error::Storage(format!("bad begun_at: {e}")))
        })?;

    let participants = match item.get("participants") {
        Some(AttributeValue::L(list)) => list
            .iter()
            .map(participant_from_av)
            .collect::<Result<Vec<_>, _>>()?,
        _ => vec![],
    };

    Ok(TransactionRecord {
        txn_id,
        lease_id,
        state,
        participants,
        begun_at,
    })
}

fn participants_to_av(participants: &[ParticipantRecord]) -> AttributeValue {
    AttributeValue::L(participants.iter().map(participant_to_av).collect())
}

// ── trait implementation ──────────────────────────────────────────────────────

#[async_trait]
impl TxnStore for DynamoTxnStore {
    async fn create(&self, record: TransactionRecord) -> Result<(), Error> {
        self.client
            .put_item()
            .table_name(&self.table_name)
            .item("txn_id", AttributeValue::S(record.txn_id))
            .item("lease_id", AttributeValue::S(record.lease_id))
            .item(
                "state",
                AttributeValue::S(state_to_str(&record.state).to_string()),
            )
            .item("begun_at", AttributeValue::S(record.begun_at.to_rfc3339()))
            .item("participants", participants_to_av(&record.participants))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("put_item failed: {e}")))?;

        Ok(())
    }

    async fn get(&self, txn_id: &str) -> Result<Option<TransactionRecord>, Error> {
        let resp = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("txn_id", AttributeValue::S(txn_id.to_string()))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("get_item failed: {e}")))?;

        match resp.item {
            None => Ok(None),
            Some(item) => Ok(Some(record_from_item(&item)?)),
        }
    }

    async fn update_state(&self, txn_id: &str, state: TransactionState) -> Result<(), Error> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("txn_id", AttributeValue::S(txn_id.to_string()))
            .update_expression("SET #s = :s")
            .expression_attribute_names("#s", "state")
            .expression_attribute_values(":s", AttributeValue::S(state_to_str(&state).to_string()))
            .condition_expression("attribute_exists(txn_id)")
            .send()
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                let is_condition_check = e
                    .as_service_error()
                    .is_some_and(|se| se.is_conditional_check_failed_exception());
                if is_condition_check {
                    Err(Error::TransactionNotFound(txn_id.to_string()))
                } else {
                    Err(Error::Storage(format!("update_item failed: {e}")))
                }
            }
        }
    }

    async fn add_participant(
        &self,
        txn_id: &str,
        participant: ParticipantRecord,
    ) -> Result<(), Error> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("txn_id", AttributeValue::S(txn_id.to_string()))
            .update_expression("SET participants = list_append(participants, :p)")
            .expression_attribute_values(
                ":p",
                AttributeValue::L(vec![participant_to_av(&participant)]),
            )
            .condition_expression("attribute_exists(txn_id)")
            .send()
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                let is_condition_check = e
                    .as_service_error()
                    .is_some_and(|se| se.is_conditional_check_failed_exception());
                if is_condition_check {
                    Err(Error::TransactionNotFound(txn_id.to_string()))
                } else {
                    Err(Error::Storage(format!("update_item failed: {e}")))
                }
            }
        }
    }

    async fn remove(&self, txn_id: &str) -> Result<(), Error> {
        self.client
            .delete_item()
            .table_name(&self.table_name)
            .key("txn_id", AttributeValue::S(txn_id.to_string()))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;
        Ok(())
    }

    async fn list_all(&self) -> Result<Vec<TransactionRecord>, Error> {
        let mut records = Vec::new();
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
                records.push(record_from_item(&item)?);
            }

            last_key = resp.last_evaluated_key;
            if last_key.is_none() {
                break;
            }
        }

        Ok(records)
    }
}

// ── integration tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::make_dynamo_client;
    use aws_sdk_dynamodb::Client;
    use uuid::Uuid;

    async fn setup() -> (DynamoTxnStore, String, Client) {
        std::env::set_var("DYNAMODB_ENDPOINT", "http://localhost:4566");
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
        std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");

        let client = make_dynamo_client().await;
        let table_name = format!("coordin8_txn_test_{}", Uuid::new_v4());

        let store = DynamoTxnStore::with_table(client.clone(), &table_name);
        store.init().await.expect("table creation failed");

        (store, table_name, client)
    }

    async fn teardown(client: &Client, table_name: &str) {
        let _ = client.delete_table().table_name(table_name).send().await;
    }

    fn make_record(txn_id: &str, lease_id: &str) -> TransactionRecord {
        TransactionRecord {
            txn_id: txn_id.to_string(),
            lease_id: lease_id.to_string(),
            state: TransactionState::Active,
            participants: vec![],
            begun_at: Utc::now(),
        }
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn create_and_get() {
        let (store, table_name, client) = setup().await;

        let record = make_record("txn-1", "lease-1");
        store.create(record).await.unwrap();

        let fetched = store.get("txn-1").await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.txn_id, "txn-1");
        assert_eq!(fetched.lease_id, "lease-1");
        assert_eq!(fetched.state, TransactionState::Active);
        assert!(fetched.participants.is_empty());

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn update_state() {
        let (store, table_name, client) = setup().await;

        store
            .create(make_record("txn-state", "lease-s"))
            .await
            .unwrap();
        store
            .update_state("txn-state", TransactionState::Committed)
            .await
            .unwrap();

        let fetched = store.get("txn-state").await.unwrap().unwrap();
        assert_eq!(fetched.state, TransactionState::Committed);

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn update_state_not_found() {
        let (store, table_name, client) = setup().await;

        let result = store
            .update_state("no-such-txn", TransactionState::Aborted)
            .await;
        assert!(result.is_err());

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn add_participant() {
        let (store, table_name, client) = setup().await;

        store
            .create(make_record("txn-part", "lease-p"))
            .await
            .unwrap();
        store
            .add_participant(
                "txn-part",
                ParticipantRecord {
                    endpoint: "localhost:8080".to_string(),
                    crash_count: 0,
                },
            )
            .await
            .unwrap();

        let fetched = store.get("txn-part").await.unwrap().unwrap();
        assert_eq!(fetched.participants.len(), 1);
        assert_eq!(fetched.participants[0].endpoint, "localhost:8080");
        assert_eq!(fetched.participants[0].crash_count, 0);

        // Add a second participant
        store
            .add_participant(
                "txn-part",
                ParticipantRecord {
                    endpoint: "localhost:8081".to_string(),
                    crash_count: 1,
                },
            )
            .await
            .unwrap();

        let fetched = store.get("txn-part").await.unwrap().unwrap();
        assert_eq!(fetched.participants.len(), 2);

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn remove() {
        let (store, table_name, client) = setup().await;

        store
            .create(make_record("txn-rm", "lease-rm"))
            .await
            .unwrap();
        store.remove("txn-rm").await.unwrap();
        assert!(store.get("txn-rm").await.unwrap().is_none());

        teardown(&client, &table_name).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn list_all() {
        let (store, table_name, client) = setup().await;

        store.create(make_record("txn-a", "lease-a")).await.unwrap();
        store.create(make_record("txn-b", "lease-b")).await.unwrap();

        let all = store.list_all().await.unwrap();
        assert_eq!(all.len(), 2);

        let ids: Vec<&str> = all.iter().map(|r| r.txn_id.as_str()).collect();
        assert!(ids.contains(&"txn-a"));
        assert!(ids.contains(&"txn-b"));

        teardown(&client, &table_name).await;
    }
}
