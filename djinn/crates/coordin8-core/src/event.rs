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
    async fn list_subscriptions(&self) -> Result<Vec<SubscriptionRecord>, Error>;

    /// Enqueue an event into a durable subscription's mailbox.
    async fn enqueue(&self, registration_id: &str, event: EventRecord) -> Result<(), Error>;

    /// Drain and return all queued events for a subscription.
    async fn dequeue(&self, registration_id: &str) -> Result<Vec<EventRecord>, Error>;
}
