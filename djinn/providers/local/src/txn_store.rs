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

    async fn update_state_if(
        &self,
        txn_id: &str,
        expected: TransactionState,
        new: TransactionState,
    ) -> Result<bool, Error> {
        match self.records.get_mut(txn_id) {
            Some(mut r) => {
                if r.state == expected {
                    r.state = new;
                    Ok(true)
                } else {
                    Ok(false)
                }
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
                if r.state != TransactionState::Active {
                    return Err(Error::TransactionTerminal(txn_id.to_string()));
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn rec(id: &str) -> TransactionRecord {
        TransactionRecord {
            txn_id: id.into(),
            lease_id: "l".into(),
            state: TransactionState::Active,
            participants: vec![],
            begun_at: Utc::now(),
        }
    }

    fn part() -> ParticipantRecord {
        ParticipantRecord {
            endpoint: "h:1".into(),
            crash_count: 0,
        }
    }

    #[tokio::test]
    async fn cas_applies_only_on_expected_state() {
        let s = InMemoryTxnStore::new();
        s.create(rec("t")).await.unwrap();
        assert!(!s
            .update_state_if("t", TransactionState::Voting, TransactionState::Committed)
            .await
            .unwrap());
        assert_eq!(
            s.get("t").await.unwrap().unwrap().state,
            TransactionState::Active
        );
        assert!(s
            .update_state_if("t", TransactionState::Active, TransactionState::Voting)
            .await
            .unwrap());
        assert_eq!(
            s.get("t").await.unwrap().unwrap().state,
            TransactionState::Voting
        );
        assert!(s
            .update_state_if("nope", TransactionState::Active, TransactionState::Voting)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn add_participant_rejected_when_not_active() {
        let s = InMemoryTxnStore::new();
        s.create(rec("t")).await.unwrap();
        s.add_participant("t", part()).await.unwrap();
        s.update_state("t", TransactionState::Voting).await.unwrap();
        let err = s.add_participant("t", part()).await.unwrap_err();
        assert!(matches!(err, Error::TransactionTerminal(_)));
        assert_eq!(s.get("t").await.unwrap().unwrap().participants.len(), 1);
    }
}
