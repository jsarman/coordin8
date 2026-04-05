use std::collections::HashMap;

use async_trait::async_trait;
use aws_sdk_dynamodb::{Client, types::AttributeValue};
use chrono::{DateTime, Utc};

use coordin8_core::{DeliveryMode, Error, EventRecord, EventStore, SubscriptionRecord};

use crate::table::{
    ensure_event_mailbox_table, ensure_event_sub_table, EVENT_MAILBOX_TABLE, EVENT_SUB_LEASE_GSI,
    EVENT_SUB_TABLE,
};

pub struct DynamoEventStore {
    client: Client,
    sub_table: String,
    mailbox_table: String,
}

impl DynamoEventStore {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            sub_table: EVENT_SUB_TABLE.to_string(),
            mailbox_table: EVENT_MAILBOX_TABLE.to_string(),
        }
    }

    pub fn with_tables(
        client: Client,
        sub_table: impl Into<String>,
        mailbox_table: impl Into<String>,
    ) -> Self {
        Self {
            client,
            sub_table: sub_table.into(),
            mailbox_table: mailbox_table.into(),
        }
    }

    pub async fn init(&self) -> Result<(), Error> {
        ensure_event_sub_table(&self.client, &self.sub_table)
            .await
            .map_err(Error::Storage)?;
        ensure_event_mailbox_table(&self.client, &self.mailbox_table)
            .await
            .map_err(Error::Storage)?;
        Ok(())
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn delivery_to_str(mode: &DeliveryMode) -> &'static str {
    match mode {
        DeliveryMode::Durable => "Durable",
        DeliveryMode::BestEffort => "BestEffort",
    }
}

fn delivery_from_str(s: &str) -> Result<DeliveryMode, Error> {
    match s {
        "Durable" => Ok(DeliveryMode::Durable),
        "BestEffort" => Ok(DeliveryMode::BestEffort),
        other => Err(Error::Storage(format!("unknown delivery mode: {other}"))),
    }
}

fn sub_from_item(item: &HashMap<String, AttributeValue>) -> Result<SubscriptionRecord, Error> {
    let registration_id = item
        .get("registration_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing registration_id".into()))?
        .clone();

    let source = item
        .get("source")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing source".into()))?
        .clone();

    let lease_id = item
        .get("lease_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing lease_id".into()))?
        .clone();

    let delivery = item
        .get("delivery")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing delivery".into()))
        .and_then(|s| delivery_from_str(s))?;

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

    let initial_seq_num = item
        .get("initial_seq_num")
        .and_then(|v| v.as_n().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    Ok(SubscriptionRecord {
        registration_id,
        source,
        template,
        delivery,
        lease_id,
        handback,
        initial_seq_num,
    })
}

fn event_from_item(item: &HashMap<String, AttributeValue>) -> Result<EventRecord, Error> {
    let event_id = item
        .get("event_id")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing event_id".into()))?
        .clone();

    let source = item
        .get("source")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing source".into()))?
        .clone();

    let event_type = item
        .get("event_type")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing event_type".into()))?
        .clone();

    let seq_num = item
        .get("seq_num")
        .and_then(|v| v.as_n().ok())
        .ok_or_else(|| Error::Storage("missing seq_num".into()))
        .and_then(|s| {
            s.parse::<u64>()
                .map_err(|e| Error::Storage(format!("bad seq_num: {e}")))
        })?;

    let emitted_at: DateTime<Utc> = item
        .get("emitted_at")
        .and_then(|v| v.as_s().ok())
        .ok_or_else(|| Error::Storage("missing emitted_at".into()))
        .and_then(|s| {
            s.parse::<DateTime<Utc>>()
                .map_err(|e| Error::Storage(format!("bad emitted_at: {e}")))
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

    Ok(EventRecord {
        event_id,
        source,
        event_type,
        seq_num,
        attrs,
        payload,
        emitted_at,
    })
}

// ── trait implementation ──────────────────────────────────────────────────────

#[async_trait]
impl EventStore for DynamoEventStore {
    async fn create_subscription(&self, sub: SubscriptionRecord) -> Result<(), Error> {
        let template_map: HashMap<String, AttributeValue> = sub
            .template
            .iter()
            .map(|(k, v)| (k.clone(), AttributeValue::S(v.clone())))
            .collect();

        let mut req = self
            .client
            .put_item()
            .table_name(&self.sub_table)
            .item(
                "registration_id",
                AttributeValue::S(sub.registration_id),
            )
            .item("source", AttributeValue::S(sub.source))
            .item("lease_id", AttributeValue::S(sub.lease_id))
            .item(
                "delivery",
                AttributeValue::S(delivery_to_str(&sub.delivery).to_string()),
            )
            .item("template", AttributeValue::M(template_map))
            .item(
                "initial_seq_num",
                AttributeValue::N(sub.initial_seq_num.to_string()),
            );

        if !sub.handback.is_empty() {
            req = req.item(
                "handback",
                AttributeValue::B(aws_sdk_dynamodb::primitives::Blob::new(sub.handback)),
            );
        }

        req.send()
            .await
            .map_err(|e| Error::Storage(format!("put_item failed: {e}")))?;

        Ok(())
    }

    async fn get_subscription(
        &self,
        registration_id: &str,
    ) -> Result<Option<SubscriptionRecord>, Error> {
        let resp = self
            .client
            .get_item()
            .table_name(&self.sub_table)
            .key(
                "registration_id",
                AttributeValue::S(registration_id.to_string()),
            )
            .send()
            .await
            .map_err(|e| Error::Storage(format!("get_item failed: {e}")))?;

        match resp.item {
            None => Ok(None),
            Some(item) => Ok(Some(sub_from_item(&item)?)),
        }
    }

    async fn remove_subscription(&self, registration_id: &str) -> Result<(), Error> {
        // Delete the subscription record
        self.client
            .delete_item()
            .table_name(&self.sub_table)
            .key(
                "registration_id",
                AttributeValue::S(registration_id.to_string()),
            )
            .send()
            .await
            .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;

        // Drain the mailbox — query all items and batch-delete them
        self.drain_mailbox(registration_id).await?;

        Ok(())
    }

    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<SubscriptionRecord>, Error> {
        // Query the GSI to find the registration_id for this lease
        let resp = self
            .client
            .query()
            .table_name(&self.sub_table)
            .index_name(EVENT_SUB_LEASE_GSI)
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

        let sub = sub_from_item(&item)?;
        let reg_id = sub.registration_id.clone();

        // Delete the subscription
        self.client
            .delete_item()
            .table_name(&self.sub_table)
            .key("registration_id", AttributeValue::S(reg_id.clone()))
            .send()
            .await
            .map_err(|e| Error::Storage(format!("delete_item failed: {e}")))?;

        // Drain the mailbox
        self.drain_mailbox(&reg_id).await?;

        Ok(Some(sub))
    }

    async fn list_subscriptions(&self) -> Result<Vec<SubscriptionRecord>, Error> {
        let mut subs = Vec::new();
        let mut last_key: Option<HashMap<String, AttributeValue>> = None;

        loop {
            let mut req = self.client.scan().table_name(&self.sub_table);
            if let Some(ref lek) = last_key {
                req = req.set_exclusive_start_key(Some(lek.clone()));
            }

            let resp = req
                .send()
                .await
                .map_err(|e| Error::Storage(format!("scan failed: {e}")))?;

            for item in resp.items.unwrap_or_default() {
                subs.push(sub_from_item(&item)?);
            }

            last_key = resp.last_evaluated_key;
            if last_key.is_none() {
                break;
            }
        }

        Ok(subs)
    }

    async fn enqueue(&self, registration_id: &str, event: EventRecord) -> Result<(), Error> {
        let attrs_map: HashMap<String, AttributeValue> = event
            .attrs
            .iter()
            .map(|(k, v)| (k.clone(), AttributeValue::S(v.clone())))
            .collect();

        let mut req = self
            .client
            .put_item()
            .table_name(&self.mailbox_table)
            .item(
                "registration_id",
                AttributeValue::S(registration_id.to_string()),
            )
            .item("seq_num", AttributeValue::N(event.seq_num.to_string()))
            .item("event_id", AttributeValue::S(event.event_id))
            .item("source", AttributeValue::S(event.source))
            .item("event_type", AttributeValue::S(event.event_type))
            .item(
                "emitted_at",
                AttributeValue::S(event.emitted_at.to_rfc3339()),
            )
            .item("attrs", AttributeValue::M(attrs_map));

        if !event.payload.is_empty() {
            req = req.item(
                "payload",
                AttributeValue::B(aws_sdk_dynamodb::primitives::Blob::new(event.payload)),
            );
        }

        req.send()
            .await
            .map_err(|e| Error::Storage(format!("put_item failed: {e}")))?;

        Ok(())
    }

    async fn dequeue(&self, registration_id: &str) -> Result<Vec<EventRecord>, Error> {
        // Query all mailbox items ordered by seq_num (sort key)
        let resp = self
            .client
            .query()
            .table_name(&self.mailbox_table)
            .key_condition_expression("registration_id = :rid")
            .expression_attribute_values(
                ":rid",
                AttributeValue::S(registration_id.to_string()),
            )
            .send()
            .await
            .map_err(|e| Error::Storage(format!("query failed: {e}")))?;

        let items = resp.items.unwrap_or_default();
        let mut events = Vec::with_capacity(items.len());

        // Parse and batch-delete
        for item in &items {
            events.push(event_from_item(item)?);
        }

        // Delete all items we just read (batch delete, 25 at a time)
        for chunk in items.chunks(25) {
            let mut batch = self
                .client
                .batch_write_item();

            let delete_requests: Vec<_> = chunk
                .iter()
                .map(|item| {
                    let mut key = HashMap::new();
                    key.insert(
                        "registration_id".to_string(),
                        item.get("registration_id").unwrap().clone(),
                    );
                    key.insert(
                        "seq_num".to_string(),
                        item.get("seq_num").unwrap().clone(),
                    );
                    aws_sdk_dynamodb::types::WriteRequest::builder()
                        .delete_request(
                            aws_sdk_dynamodb::types::DeleteRequest::builder()
                                .set_key(Some(key))
                                .build()
                                .unwrap(),
                        )
                        .build()
                })
                .collect();

            batch = batch.request_items(&self.mailbox_table, delete_requests);
            batch
                .send()
                .await
                .map_err(|e| Error::Storage(format!("batch_write_item failed: {e}")))?;
        }

        Ok(events)
    }
}

impl DynamoEventStore {
    /// Delete all mailbox items for a registration_id.
    async fn drain_mailbox(&self, registration_id: &str) -> Result<(), Error> {
        loop {
            let resp = self
                .client
                .query()
                .table_name(&self.mailbox_table)
                .key_condition_expression("registration_id = :rid")
                .expression_attribute_values(
                    ":rid",
                    AttributeValue::S(registration_id.to_string()),
                )
                .limit(25)
                .send()
                .await
                .map_err(|e| Error::Storage(format!("query failed: {e}")))?;

            let items = resp.items.unwrap_or_default();
            if items.is_empty() {
                break;
            }

            let delete_requests: Vec<_> = items
                .iter()
                .map(|item| {
                    let mut key = HashMap::new();
                    key.insert(
                        "registration_id".to_string(),
                        item.get("registration_id").unwrap().clone(),
                    );
                    key.insert(
                        "seq_num".to_string(),
                        item.get("seq_num").unwrap().clone(),
                    );
                    aws_sdk_dynamodb::types::WriteRequest::builder()
                        .delete_request(
                            aws_sdk_dynamodb::types::DeleteRequest::builder()
                                .set_key(Some(key))
                                .build()
                                .unwrap(),
                        )
                        .build()
                })
                .collect();

            self.client
                .batch_write_item()
                .request_items(&self.mailbox_table, delete_requests)
                .send()
                .await
                .map_err(|e| Error::Storage(format!("batch_write_item failed: {e}")))?;

            if items.len() < 25 {
                break;
            }
        }

        Ok(())
    }
}

// ── integration tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::make_dynamo_client;
    use aws_sdk_dynamodb::Client;
    use uuid::Uuid;

    async fn setup() -> (DynamoEventStore, String, String, Client) {
        std::env::set_var("DYNAMODB_ENDPOINT", "http://localhost:4566");
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
        std::env::set_var("AWS_DEFAULT_REGION", "us-east-1");

        let client = make_dynamo_client().await;
        let suffix = Uuid::new_v4();
        let sub_table = format!("coordin8_event_sub_test_{suffix}");
        let mailbox_table = format!("coordin8_event_mailbox_test_{suffix}");

        let store =
            DynamoEventStore::with_tables(client.clone(), &sub_table, &mailbox_table);
        store.init().await.expect("table creation failed");

        (store, sub_table, mailbox_table, client)
    }

    async fn teardown(client: &Client, sub_table: &str, mailbox_table: &str) {
        let _ = client.delete_table().table_name(sub_table).send().await;
        let _ = client
            .delete_table()
            .table_name(mailbox_table)
            .send()
            .await;
    }

    fn make_sub(reg_id: &str, lease_id: &str) -> SubscriptionRecord {
        let mut template = HashMap::new();
        template.insert("event_type".to_string(), "price_update".to_string());

        SubscriptionRecord {
            registration_id: reg_id.to_string(),
            source: "market-data".to_string(),
            template,
            delivery: DeliveryMode::Durable,
            lease_id: lease_id.to_string(),
            handback: vec![1, 2, 3],
            initial_seq_num: 0,
        }
    }

    fn make_event(event_id: &str, seq: u64) -> EventRecord {
        EventRecord {
            event_id: event_id.to_string(),
            source: "market-data".to_string(),
            event_type: "price_update".to_string(),
            seq_num: seq,
            attrs: HashMap::new(),
            payload: vec![42],
            emitted_at: Utc::now(),
        }
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn create_and_get_subscription() {
        let (store, sub_table, mailbox_table, client) = setup().await;

        let sub = make_sub("reg-1", "lease-1");
        store.create_subscription(sub).await.unwrap();

        let fetched = store.get_subscription("reg-1").await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.registration_id, "reg-1");
        assert_eq!(fetched.lease_id, "lease-1");
        assert_eq!(fetched.source, "market-data");
        assert_eq!(fetched.delivery, DeliveryMode::Durable);
        assert_eq!(fetched.handback, vec![1, 2, 3]);

        teardown(&client, &sub_table, &mailbox_table).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn remove_subscription() {
        let (store, sub_table, mailbox_table, client) = setup().await;

        store.create_subscription(make_sub("reg-rm", "lease-rm")).await.unwrap();
        store.remove_subscription("reg-rm").await.unwrap();
        assert!(store.get_subscription("reg-rm").await.unwrap().is_none());

        teardown(&client, &sub_table, &mailbox_table).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn remove_by_lease() {
        let (store, sub_table, mailbox_table, client) = setup().await;

        store
            .create_subscription(make_sub("reg-lease", "lease-target"))
            .await
            .unwrap();

        let removed = store.remove_by_lease("lease-target").await.unwrap();
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().registration_id, "reg-lease");

        assert!(store.get_subscription("reg-lease").await.unwrap().is_none());

        teardown(&client, &sub_table, &mailbox_table).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn enqueue_and_dequeue() {
        let (store, sub_table, mailbox_table, client) = setup().await;

        store
            .create_subscription(make_sub("reg-q", "lease-q"))
            .await
            .unwrap();

        store.enqueue("reg-q", make_event("e1", 1)).await.unwrap();
        store.enqueue("reg-q", make_event("e2", 2)).await.unwrap();
        store.enqueue("reg-q", make_event("e3", 3)).await.unwrap();

        let events = store.dequeue("reg-q").await.unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].seq_num, 1);
        assert_eq!(events[1].seq_num, 2);
        assert_eq!(events[2].seq_num, 3);

        // Second dequeue should be empty — events were consumed
        let events = store.dequeue("reg-q").await.unwrap();
        assert!(events.is_empty());

        teardown(&client, &sub_table, &mailbox_table).await;
    }

    #[tokio::test]
    #[ignore = "requires MiniStack on localhost:4566"]
    async fn list_subscriptions() {
        let (store, sub_table, mailbox_table, client) = setup().await;

        store.create_subscription(make_sub("reg-a", "lease-a")).await.unwrap();
        store.create_subscription(make_sub("reg-b", "lease-b")).await.unwrap();

        let all = store.list_subscriptions().await.unwrap();
        assert_eq!(all.len(), 2);

        teardown(&client, &sub_table, &mailbox_table).await;
    }
}
