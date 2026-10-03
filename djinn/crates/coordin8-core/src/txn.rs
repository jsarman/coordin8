use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum TransactionState {
    Active,
    Voting,
    Prepared,
    Committed,
    Aborted,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrepareVote {
    Prepared,
    NotChanged,
    Aborted,
}

/// A participant registered via Enlist — holds the endpoint the TxnMgr
/// calls back during 2PC.
#[derive(Debug, Clone)]
pub struct ParticipantRecord {
    pub endpoint: String, // host:port of the participant's ParticipantService
    pub crash_count: u64, // monotonic counter; prevents stale rejoin
}

/// One active or terminal transaction.
#[derive(Debug, Clone)]
pub struct TransactionRecord {
    pub txn_id: String,
    pub lease_id: String,
    pub state: TransactionState,
    pub participants: Vec<ParticipantRecord>,
    pub begun_at: DateTime<Utc>,
}

/// Abstract enlist client for participants.
///
/// Mirrors the `Leasing` and `CapabilityResolver` seam patterns: services that
/// need to enlist as 2PC participants take an `Arc<dyn TxnEnlister>` and don't
/// care whether the TxnMgr is in-process or across the network.
///
/// The bundled Djinn provides a local impl that calls into `TxnManager::enlist`
/// directly. Split-mode services hand out a remote impl that forwards each call
/// to a `TxnMgr` instance discovered through Registry.
///
/// Only `enlist` is on the trait — services don't need begin/commit/abort, those
/// are driven by application code or the lease-expiry cascade.
#[async_trait]
pub trait TxnEnlister: Send + Sync {
    async fn enlist(&self, txn_id: &str, endpoint: &str) -> Result<(), Error>;
}

#[async_trait]
pub trait TxnStore: Send + Sync {
    async fn create(&self, record: TransactionRecord) -> Result<(), Error>;
    async fn get(&self, txn_id: &str) -> Result<Option<TransactionRecord>, Error>;
    async fn update_state(&self, txn_id: &str, state: TransactionState) -> Result<(), Error>;
    /// Atomic compare-and-set on the transaction state. Returns `Ok(true)` if
    /// the state was `expected` and is now `new`; `Ok(false)` if the state
    /// differed (nothing written). `TransactionNotFound` if the txn is missing.
    async fn update_state_if(
        &self,
        txn_id: &str,
        expected: TransactionState,
        new: TransactionState,
    ) -> Result<bool, Error>;
    /// Append a participant, atomically with a check that the txn is `Active`.
    /// Returns `TransactionTerminal` if the state is anything else, and
    /// `TransactionNotFound` if the txn is missing.
    async fn add_participant(
        &self,
        txn_id: &str,
        participant: ParticipantRecord,
    ) -> Result<(), Error>;
    async fn remove(&self, txn_id: &str) -> Result<(), Error>;
    async fn list_all(&self) -> Result<Vec<TransactionRecord>, Error>;
}
