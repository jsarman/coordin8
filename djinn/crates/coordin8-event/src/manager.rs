use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::broadcast;
use tracing::{debug, error};
use uuid::Uuid;

use coordin8_core::{
    DeliveryMode, Error, EventRecord, EventStore, LeaseRecord, Leasing, SubscriptionRecord,
};
use coordin8_registry::matcher::{matches, parse_template};

/// # Delivery contract
///
/// * **Durable** subscriptions are *at-least-once*. Every matching event is
///   persisted into the subscription's mailbox with a per-registration,
///   monotonically increasing sequence number (the `seq_num` the client sees),
///   and `Receive` removes (acks) an entry only after it has been handed to the
///   gRPC stream. A crash or disconnect between send and ack may redeliver, so
///   clients de-duplicate on the per-registration `seq_num`.
/// * **BestEffort** subscriptions get only the live broadcast, stamped with the
///   per `source::event_type` counter (in-memory; resets on restart). A
///   subscriber that falls behind is terminated with `DATA_LOSS`.
pub struct EventManager {
    store: Arc<dyn EventStore>,
    lease_manager: Arc<dyn Leasing>,
    event_tx: broadcast::Sender<EventRecord>,
    /// Sequence counter per "source::event_type" key.
    seq_counters: DashMap<String, AtomicU64>,
}

impl EventManager {
    pub fn new(
        store: Arc<dyn EventStore>,
        lease_manager: Arc<dyn Leasing>,
        event_tx: broadcast::Sender<EventRecord>,
    ) -> Self {
        Self {
            store,
            lease_manager,
            event_tx,
            seq_counters: DashMap::new(),
        }
    }

    /// Create a subscription. Returns (registration_id, lease, initial_seq_num).
    pub async fn subscribe(
        &self,
        source: String,
        template: HashMap<String, String>,
        delivery: DeliveryMode,
        ttl_secs: u64,
        handback: Vec<u8>,
    ) -> Result<(String, LeaseRecord, u64), Error> {
        let registration_id = Uuid::new_v4().to_string();
        let resource_id = format!("event:{}", registration_id);
        let lease = self.lease_manager.grant(&resource_id, ttl_secs).await?;

        // Durable seqs are per-registration and start at 1, so the baseline is
        // 0. BestEffort snapshots the current source+type counter.
        let initial_seq_num = if delivery == DeliveryMode::Durable {
            0
        } else {
            self.seq_counters
                .iter()
                .filter(|r| r.key().starts_with(&format!("{}::", source)))
                .map(|r| r.value().load(Ordering::SeqCst))
                .max()
                .unwrap_or(0)
        };

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

        Ok((registration_id, lease, initial_seq_num))
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

        // Persist into durable mailboxes FIRST, then broadcast, so a durable
        // receiver woken by the broadcast always finds the entry in its mailbox.
        //
        // A failed enqueue must not be silent: the producer is told (Err) so it
        // can retry. We still attempt every subscription and still broadcast,
        // so one bad mailbox doesn't starve the others; a retry may therefore
        // duplicate to subscriptions that already succeeded (at-least-once).
        let subs = self.store.list_subscriptions().await?;
        let mut failures: Vec<String> = Vec::new();
        for sub in subs {
            if sub.source != source || sub.delivery != DeliveryMode::Durable {
                continue;
            }
            let ops = parse_template(&sub.template);
            let mut check_attrs = attrs.clone();
            check_attrs.insert("event_type".to_string(), event_type.clone());
            if ops.is_empty() || matches(&ops, &check_attrs) {
                if let Err(e) = self
                    .store
                    .enqueue(&sub.registration_id, event.clone())
                    .await
                {
                    // A subscription removed between list and enqueue is not a failure.
                    if matches!(e, Error::SubscriptionNotFound(_)) {
                        continue;
                    }
                    error!(
                        registration_id = %sub.registration_id,
                        event_id = %event.event_id,
                        error = %e,
                        "durable enqueue failed"
                    );
                    failures.push(format!("{}: {e}", sub.registration_id));
                }
            }
        }

        // Live receivers: BestEffort get the event itself; Durable receivers
        // treat it as a wake-up to re-read their mailbox.
        let _ = self.event_tx.send(event.clone());

        if !failures.is_empty() {
            return Err(Error::Storage(format!(
                "durable enqueue failed for {} subscription(s): {}",
                failures.len(),
                failures.join("; ")
            )));
        }

        Ok(event)
    }

    pub async fn get_subscription(
        &self,
        registration_id: &str,
    ) -> Result<Option<SubscriptionRecord>, Error> {
        self.store.get_subscription(registration_id).await
    }

    /// Read (without removing) up to `limit` queued events with
    /// per-registration seq greater than `after_seq`.
    pub async fn peek_mailbox(
        &self,
        registration_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<EventRecord>, Error> {
        self.store.peek(registration_id, after_seq, limit).await
    }

    /// Acknowledge (delete) delivered mailbox entries.
    pub async fn ack_mailbox(&self, registration_id: &str, seq_nums: &[u64]) -> Result<(), Error> {
        self.store.ack(registration_id, seq_nums).await
    }

    /// Subscribe to the live broadcast channel.
    pub fn subscribe_broadcast(&self) -> broadcast::Receiver<EventRecord> {
        self.event_tx.subscribe()
    }

    /// Remove the subscription associated with the given lease ID.
    /// Called by the lease expiry cascade — no need to cancel the lease itself
    /// since it's already expired.
    pub async fn unsubscribe_by_lease(&self, lease_id: &str) -> Result<Option<String>, Error> {
        let removed = self.store.remove_by_lease(lease_id).await?;
        if let Some(ref sub) = removed {
            debug!(
                registration_id = %sub.registration_id,
                lease_id,
                "subscription expired (lease cascade)"
            );
        }
        Ok(removed.map(|s| s.registration_id))
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
