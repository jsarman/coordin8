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
    /// Insert a tuple into the committed (visible) store.
    async fn insert(&self, record: TupleRecord) -> Result<(), Error>;

    /// Insert a tuple into the uncommitted buffer for a transaction.
    /// Invisible to non-transactional reads; visible only within the same txn.
    async fn insert_uncommitted(&self, txn_id: &str, record: TupleRecord) -> Result<(), Error>;

    /// Get a tuple by ID (non-destructive).
    async fn get(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error>;

    /// Remove a tuple by ID. Returns the removed tuple if found.
    async fn remove(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error>;

    /// Remove a tuple by its lease ID. Returns the removed tuple if found.
    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<TupleRecord>, Error>;

    /// Find the first tuple matching the template (non-destructive).
    /// When txn_id is Some, also searches that transaction's uncommitted buffer.
    async fn find_match(
        &self,
        template: &HashMap<String, String>,
        txn_id: Option<&str>,
    ) -> Result<Option<TupleRecord>, Error>;

    /// Atomically find and remove the first tuple matching the template.
    /// When txn_id is Some, searches uncommitted first, then committed.
    /// Takes from committed under a txn are held for restore-on-abort.
    async fn take_match(
        &self,
        template: &HashMap<String, String>,
        txn_id: Option<&str>,
    ) -> Result<Option<TupleRecord>, Error>;

    /// Find all tuples matching the template (non-destructive).
    /// When txn_id is Some, includes that transaction's uncommitted tuples.
    async fn find_all_matches(
        &self,
        template: &HashMap<String, String>,
        txn_id: Option<&str>,
    ) -> Result<Vec<TupleRecord>, Error>;

    /// Commit a transaction: flush uncommitted writes to the visible store,
    /// finalize takes (stay removed). Returns the flushed tuples for broadcasting.
    async fn commit_txn(&self, txn_id: &str) -> Result<Vec<TupleRecord>, Error>;

    /// Abort a transaction: discard uncommitted writes, restore taken tuples.
    /// Returns (discarded_uncommitted, restored_taken).
    /// Discarded tuples need lease cleanup; restored tuples need broadcasting.
    async fn abort_txn(&self, txn_id: &str) -> Result<(Vec<TupleRecord>, Vec<TupleRecord>), Error>;

    /// Check if this store has any uncommitted state for the given transaction.
    async fn has_txn(&self, txn_id: &str) -> Result<bool, Error>;

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
