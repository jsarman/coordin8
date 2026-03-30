use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Error;

/// A tuple stored in the Space.
#[derive(Debug, Clone)]
pub struct TupleRecord {
    pub tuple_id: String,
    pub attrs: HashMap<String, String>,
    pub payload: Vec<u8>,
    pub lease_id: String,
    pub written_by: String,
    pub written_at: DateTime<Utc>,
    pub input_tuple_id: Option<String>,
}

/// A leased watch subscription on the Space.
#[derive(Debug, Clone)]
pub struct SpaceWatchRecord {
    pub watch_id: String,
    pub template: HashMap<String, String>,
    pub on: SpaceEventKind,
    pub lease_id: String,
    pub handback: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpaceEventKind {
    Appearance,
    Expiration,
}

/// Backing store for tuples and watches.
#[async_trait]
pub trait SpaceStore: Send + Sync {
    /// Insert a tuple into the store.
    async fn insert(&self, record: TupleRecord) -> Result<(), Error>;

    /// Get a tuple by ID (non-destructive).
    async fn get(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error>;

    /// Remove a tuple by ID. Returns the removed tuple if found.
    async fn remove(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error>;

    /// Remove a tuple by its lease ID. Returns the removed tuple if found.
    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<TupleRecord>, Error>;

    /// Find the first tuple matching the template (non-destructive).
    async fn find_match(
        &self,
        template: &HashMap<String, String>,
    ) -> Result<Option<TupleRecord>, Error>;

    /// Atomically find and remove the first tuple matching the template.
    async fn take_match(
        &self,
        template: &HashMap<String, String>,
    ) -> Result<Option<TupleRecord>, Error>;

    /// Find all tuples matching the template (non-destructive).
    async fn find_all_matches(
        &self,
        template: &HashMap<String, String>,
    ) -> Result<Vec<TupleRecord>, Error>;

    /// List all tuples (for debugging/tests).
    async fn list_all(&self) -> Result<Vec<TupleRecord>, Error>;

    /// Create a watch subscription.
    async fn create_watch(&self, watch: SpaceWatchRecord) -> Result<(), Error>;

    /// Remove a watch subscription.
    async fn remove_watch(&self, watch_id: &str) -> Result<(), Error>;

    /// Remove a watch by its lease ID.
    async fn remove_watch_by_lease(&self, lease_id: &str) -> Result<Option<SpaceWatchRecord>, Error>;

    /// List all active watches.
    async fn list_watches(&self) -> Result<Vec<SpaceWatchRecord>, Error>;
}
