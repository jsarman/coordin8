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

#[async_trait]
pub trait TxnStore: Send + Sync {
    async fn create(&self, record: TransactionRecord) -> Result<(), Error>;
    async fn get(&self, txn_id: &str) -> Result<Option<TransactionRecord>, Error>;
    async fn update_state(&self, txn_id: &str, state: TransactionState) -> Result<(), Error>;
    async fn add_participant(
        &self,
        txn_id: &str,
        participant: ParticipantRecord,
    ) -> Result<(), Error>;
    async fn remove(&self, txn_id: &str) -> Result<(), Error>;
    async fn list_all(&self) -> Result<Vec<TransactionRecord>, Error>;
}
