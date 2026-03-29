use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::broadcast;
use tracing::debug;
use uuid::Uuid;

use coordin8_core::{DeliveryMode, Error, EventRecord, EventStore, LeaseRecord, SubscriptionRecord};
use coordin8_lease::LeaseManager;
use coordin8_registry::matcher::{matches, parse_template};

pub struct EventManager {
    store: Arc<dyn EventStore>,
    lease_manager: Arc<LeaseManager>,
    event_tx: broadcast::Sender<EventRecord>,
    /// Sequence counter per "source::event_type" key.
    seq_counters: DashMap<String, AtomicU64>,
}

impl EventManager {
    pub fn new(
        store: Arc<dyn EventStore>,
        lease_manager: Arc<LeaseManager>,
        event_tx: broadcast::Sender<EventRecord>,
    ) -> Self {
        Self {
            store,
            lease_manager,
            event_tx,
            seq_counters: DashMap::new(),
        }
    }

    /// Create a subscription. Returns (registration_id, lease_id, initial_seq_num).
    pub async fn subscribe(
        &self,
        source: String,
        template: HashMap<String, String>,
        delivery: DeliveryMode,
        ttl_secs: u64,
        handback: Vec<u8>,
    ) -> Result<(String, String, u64), Error> {
        let registration_id = Uuid::new_v4().to_string();
        let resource_id = format!("event:{}", registration_id);
        let lease = self.lease_manager.grant(&resource_id, ttl_secs).await?;

        // Snapshot the current global seq for this source at subscribe time.
        let initial_seq_num = self
            .seq_counters
            .iter()
            .filter(|r| r.key().starts_with(&format!("{}::", source)))
            .map(|r| r.value().load(Ordering::SeqCst))
            .max()
            .unwrap_or(0);

        let sub = SubscriptionRecord {
            registration_id: registration_id.clone(),
            source: source.clone(),
            template,
            delivery,
            lease_id: lease.lease_id.clone(),
            handback,
            initial_seq_num,
        };

        self.store.create_subscription(sub).await?;

        debug!(
            registration_id,
            source,
            lease_id = %lease.lease_id,
            ttl_secs,
            "event subscription created"
        );

        Ok((registration_id, lease.lease_id, initial_seq_num))
    }

    /// Emit an event. Broadcasts to live receivers and enqueues into durable mailboxes.
    pub async fn emit(
        &self,
        source: String,
        event_type: String,
        attrs: HashMap<String, String>,
        payload: Vec<u8>,
    ) -> Result<EventRecord, Error> {
        let key = format!("{}::{}", source, event_type);
        let seq_num = {
            let counter = self
                .seq_counters
                .entry(key)
                .or_insert_with(|| AtomicU64::new(0));
            counter.fetch_add(1, Ordering::SeqCst) + 1
        };

        let event = EventRecord {
            event_id: Uuid::new_v4().to_string(),
            source: source.clone(),
            event_type: event_type.clone(),
            seq_num,
            attrs: attrs.clone(),
            payload,
            emitted_at: Utc::now(),
        };

        debug!(
            event_id = %event.event_id,
            source,
            event_type,
            seq_num,
            "event emitted"
        );

        // Broadcast to all active receivers (best-effort and durable alike).
        let _ = self.event_tx.send(event.clone());

        // Enqueue into durable mailboxes for matching subscriptions.
        let subs = self.store.list_subscriptions().await?;
        for sub in subs {
            if sub.source != source || sub.delivery != DeliveryMode::Durable {
                continue;
            }
            let ops = parse_template(&sub.template);
            let mut check_attrs = attrs.clone();
            check_attrs.insert("event_type".to_string(), event_type.clone());
            if ops.is_empty() || matches(&ops, &check_attrs) {
                let _ = self.store.enqueue(&sub.registration_id, event.clone()).await;
            }
        }

        Ok(event)
    }

    pub async fn get_subscription(
        &self,
        registration_id: &str,
    ) -> Result<Option<SubscriptionRecord>, Error> {
        self.store.get_subscription(registration_id).await
    }

    /// Drain all queued events from a durable mailbox.
    pub async fn drain_mailbox(&self, registration_id: &str) -> Result<Vec<EventRecord>, Error> {
        self.store.dequeue(registration_id).await
    }

    /// Subscribe to the live broadcast channel.
    pub fn subscribe_broadcast(&self) -> broadcast::Receiver<EventRecord> {
        self.event_tx.subscribe()
    }

    pub async fn cancel_subscription(&self, registration_id: &str) -> Result<(), Error> {
        let sub = self
            .store
            .get_subscription(registration_id)
            .await?
            .ok_or_else(|| Error::SubscriptionNotFound(registration_id.to_string()))?;

        self.store.remove_subscription(registration_id).await?;
        self.lease_manager.cancel(&sub.lease_id).await?;

        debug!(registration_id, "event subscription cancelled");
        Ok(())
    }

    pub async fn renew_subscription(
        &self,
        registration_id: &str,
        ttl_secs: u64,
    ) -> Result<LeaseRecord, Error> {
        let sub = self
            .store
            .get_subscription(registration_id)
            .await?
            .ok_or_else(|| Error::SubscriptionNotFound(registration_id.to_string()))?;

        self.lease_manager.renew(&sub.lease_id, ttl_secs).await
    }
}
