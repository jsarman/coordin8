use std::time::{Duration, Instant};

use async_trait::async_trait;
use dashmap::DashMap;

use coordin8_core::{
    Error, ParticipantRecord, TransactionRecord, TransactionState, TxnStore,
    FINISHED_TXN_RETENTION_SECS,
};

pub struct InMemoryTxnStore {
    records: DashMap<String, TransactionRecord>,
    /// When each finished (Committed/Aborted) record reached that state.
    /// Records older than `retention` are dropped on the next `create`,
    /// mirroring the Dynamo provider's native TTL.
    finished: DashMap<String, Instant>,
    retention: Duration,
}

impl InMemoryTxnStore {
    pub fn new() -> Self {
        Self::with_retention(Duration::from_secs(FINISHED_TXN_RETENTION_SECS))
    }

    pub fn with_retention(retention: Duration) -> Self {
        Self {
            records: DashMap::new(),
            finished: DashMap::new(),
            retention,
        }
    }

    fn note_state(&self, txn_id: &str, state: &TransactionState) {
        if matches!(
            state,
            TransactionState::Committed | TransactionState::Aborted
        ) {
            self.finished.insert(txn_id.to_string(), Instant::now());
        }
    }

    fn prune_finished(&self) {
        let retention = self.retention;
        let expired: Vec<String> = self
            .finished
            .iter()
            .filter(|e| e.value().elapsed() >= retention)
            .map(|e| e.key().clone())
            .collect();
        for id in expired {
            self.finished.remove(&id);
            self.records.remove(&id);
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
        self.prune_finished();
        self.records.insert(record.txn_id.clone(), record);
        Ok(())
    }

    async fn get(&self, txn_id: &str) -> Result<Option<TransactionRecord>, Error> {
        Ok(self.records.get(txn_id).map(|r| r.clone()))
    }

    async fn update_state(&self, txn_id: &str, state: TransactionState) -> Result<(), Error> {
        match self.records.get_mut(txn_id) {
            Some(mut r) => {
                self.note_state(txn_id, &state);
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
                    self.note_state(txn_id, &new);
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
                // A repeat enlist of the same endpoint is a no-op.
                if !r
                    .participants
                    .iter()
                    .any(|p| p.endpoint == participant.endpoint)
                {
                    r.participants.push(participant);
                }
                Ok(())
            }
            None => Err(Error::TransactionNotFound(txn_id.to_string())),
        }
    }

    async fn remove(&self, txn_id: &str) -> Result<(), Error> {
        self.finished.remove(txn_id);
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
    async fn finished_records_expire_active_ones_do_not() {
        let s = InMemoryTxnStore::with_retention(Duration::from_millis(50));
        s.create(rec("done")).await.unwrap();
        s.create(rec("open")).await.unwrap();
        s.update_state("done", TransactionState::Committed)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        s.create(rec("next")).await.unwrap();
        assert!(s.get("done").await.unwrap().is_none());
        assert!(s.get("open").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn repeat_enlist_is_deduped() {
        let s = InMemoryTxnStore::new();
        s.create(rec("t")).await.unwrap();
        s.add_participant("t", part()).await.unwrap();
        s.add_participant("t", part()).await.unwrap();
        assert_eq!(s.get("t").await.unwrap().unwrap().participants.len(), 1);
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
