use async_trait::async_trait;
use dashmap::DashMap;

use coordin8_core::{Error, ParticipantRecord, TransactionRecord, TransactionState, TxnStore};

pub struct InMemoryTxnStore {
    records: DashMap<String, TransactionRecord>,
}

impl InMemoryTxnStore {
    pub fn new() -> Self {
        Self {
            records: DashMap::new(),
        }
    }
}

impl Default for InMemoryTxnStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TxnStore for InMemoryTxnStore {
    async fn create(&self, record: TransactionRecord) -> Result<(), Error> {
        self.records.insert(record.txn_id.clone(), record);
        Ok(())
    }

    async fn get(&self, txn_id: &str) -> Result<Option<TransactionRecord>, Error> {
        Ok(self.records.get(txn_id).map(|r| r.clone()))
    }

    async fn update_state(&self, txn_id: &str, state: TransactionState) -> Result<(), Error> {
        match self.records.get_mut(txn_id) {
            Some(mut r) => {
                r.state = state;
                Ok(())
            }
            None => Err(Error::TransactionNotFound(txn_id.to_string())),
        }
    }

    async fn add_participant(
        &self,
        txn_id: &str,
        participant: ParticipantRecord,
    ) -> Result<(), Error> {
        match self.records.get_mut(txn_id) {
            Some(mut r) => {
                r.participants.push(participant);
                Ok(())
            }
            None => Err(Error::TransactionNotFound(txn_id.to_string())),
        }
    }

    async fn remove(&self, txn_id: &str) -> Result<(), Error> {
        self.records.remove(txn_id);
        Ok(())
    }

    async fn list_all(&self) -> Result<Vec<TransactionRecord>, Error> {
        Ok(self.records.iter().map(|r| r.clone()).collect())
    }
}
