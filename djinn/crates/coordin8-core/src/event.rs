use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Error;

/// A single emitted event.
#[derive(Debug, Clone)]
pub struct EventRecord {
    pub event_id: String,
    pub source: String,
    pub event_type: String,
    pub seq_num: u64,
    pub attrs: HashMap<String, String>,
    pub payload: Vec<u8>,
    pub emitted_at: DateTime<Utc>,
}

/// A leased subscription.
#[derive(Debug, Clone)]
pub struct SubscriptionRecord {
    pub registration_id: String,
    pub source: String,
    pub template: HashMap<String, String>,
    pub delivery: DeliveryMode,
    pub lease_id: String,
    pub handback: Vec<u8>,
    pub initial_seq_num: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryMode {
    Durable,
    BestEffort,
}

/// Backing store for event subscriptions and durable mailboxes.
#[async_trait]
pub trait EventStore: Send + Sync {
    async fn create_subscription(&self, sub: SubscriptionRecord) -> Result<(), Error>;
    async fn get_subscription(
        &self,
        registration_id: &str,
    ) -> Result<Option<SubscriptionRecord>, Error>;
    async fn remove_subscription(&self, registration_id: &str) -> Result<(), Error>;
    /// Remove the subscription associated with the given lease ID.
    /// Returns the removed subscription, if any.
    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<SubscriptionRecord>, Error>;
    async fn list_subscriptions(&self) -> Result<Vec<SubscriptionRecord>, Error>;

    /// Enqueue an event into a durable subscription's mailbox.
    ///
    /// The store assigns the mailbox entry a per-registration sequence number
    /// (monotonic, persisted with the subscription so it survives restarts and
    /// is never reused) and returns it. The stored event's `seq_num` is that
    /// per-registration sequence, overriding whatever the caller set.
    /// Errors with `SubscriptionNotFound` if the registration doesn't exist.
    async fn enqueue(&self, registration_id: &str, event: EventRecord) -> Result<u64, Error>;

    /// Read up to `limit` queued events with per-registration seq strictly
    /// greater than `after_seq`, in ascending seq order, WITHOUT removing
    /// them. Unknown registrations yield an empty list.
    async fn peek(
        &self,
        registration_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<EventRecord>, Error>;

    /// Delete (acknowledge) the mailbox entries with the given
    /// per-registration seqs. Idempotent; unknown entries are ignored.
    async fn ack(&self, registration_id: &str, seq_nums: &[u64]) -> Result<(), Error>;
}
