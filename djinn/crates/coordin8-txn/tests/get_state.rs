//! GetState is how a participant learns a transaction's outcome; NOT_FOUND
//! tells it "no record, abort". Only a missing record may say that
//! (review 2026-11-14 round 3: a store error read as NOT_FOUND made Space
//! drop a live transaction's writes).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use coordin8_core::{
    Error, Leasing, ParticipantRecord, TransactionRecord, TransactionState, TxnStore,
};
use coordin8_lease::LeaseManager;
use coordin8_proto::coordin8::transaction_service_server::TransactionService;
use coordin8_proto::coordin8::GetStateRequest;
use coordin8_provider_local::{InMemoryLeaseStore, InMemoryTxnStore};
use coordin8_txn::{TxnManager, TxnServiceImpl};
use tonic::{Code, Request};

/// An in-memory store whose reads can be made to fail.
#[derive(Default)]
struct FailingGets {
    real: InMemoryTxnStore,
    fail: AtomicBool,
}

#[tonic::async_trait]
impl TxnStore for FailingGets {
    async fn create(&self, r: TransactionRecord) -> Result<(), Error> {
        self.real.create(r).await
    }
    async fn get(&self, id: &str) -> Result<Option<TransactionRecord>, Error> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::Storage("read failed".into()));
        }
        self.real.get(id).await
    }
    async fn update_state(&self, id: &str, s: TransactionState) -> Result<(), Error> {
        self.real.update_state(id, s).await
    }
    async fn update_state_if(
        &self,
        id: &str,
        expected: TransactionState,
        new: TransactionState,
    ) -> Result<bool, Error> {
        self.real.update_state_if(id, expected, new).await
    }
    async fn add_participant(&self, id: &str, p: ParticipantRecord) -> Result<(), Error> {
        self.real.add_participant(id, p).await
    }
    async fn remove(&self, id: &str) -> Result<(), Error> {
        self.real.remove(id).await
    }
    async fn list_all(&self) -> Result<Vec<TransactionRecord>, Error> {
        self.real.list_all().await
    }
}

#[tokio::test]
async fn store_error_is_not_reported_as_not_found() {
    let (tx, _) = tokio::sync::broadcast::channel(256);
    let lm: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
        Arc::new(InMemoryLeaseStore::new()),
        coordin8_core::LeaseConfig::default(),
        tx,
    ));
    let store = Arc::new(FailingGets::default());
    let mgr = Arc::new(TxnManager::new(store.clone(), lm));
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    let svc = TxnServiceImpl::new(mgr, "127.0.0.1", 9004);
    let ask = |id: &str| {
        Request::new(GetStateRequest {
            txn_id: id.to_string(),
        })
    };

    assert!(svc.get_state(ask(&txn_id)).await.is_ok());
    assert_eq!(
        svc.get_state(ask("no-such-txn")).await.unwrap_err().code(),
        Code::NotFound
    );

    store.fail.store(true, Ordering::SeqCst);
    let code = svc.get_state(ask(&txn_id)).await.unwrap_err().code();
    assert_ne!(
        code,
        Code::NotFound,
        "a failed read is not a missing record"
    );
}
