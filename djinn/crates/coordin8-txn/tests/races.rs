//! Regression tests for 2PC state races (CAS-guarded transitions).
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

use coordin8_core::{Error, Leasing, TransactionState, TxnStore};
use coordin8_lease::LeaseManager;
use coordin8_proto::coordin8::{
    participant_service_server::{ParticipantService, ParticipantServiceServer},
    ParticipantRequest, PrepareResponse,
};
use coordin8_provider_local::{InMemoryLeaseStore, InMemoryTxnStore};
use coordin8_txn::TxnManager;

/// Participant whose PrepareAndCommit is slow, to hold a commit in `Voting`.
struct SlowParticipant {
    delay: Duration,
}

#[tonic::async_trait]
impl ParticipantService for SlowParticipant {
    async fn prepare(
        &self,
        _: Request<ParticipantRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        Ok(Response::new(PrepareResponse { vote: 0 }))
    }
    async fn commit(&self, _: Request<ParticipantRequest>) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }
    async fn abort(&self, _: Request<ParticipantRequest>) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }
    async fn prepare_and_commit(
        &self,
        _: Request<ParticipantRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        tokio::time::sleep(self.delay).await;
        Ok(Response::new(PrepareResponse { vote: 0 }))
    }
}

fn make_manager() -> (Arc<TxnManager>, Arc<InMemoryTxnStore>) {
    let lease_store = Arc::new(InMemoryLeaseStore::new());
    let (tx, _) = tokio::sync::broadcast::channel(256);
    let lm: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
        lease_store,
        coordin8_core::LeaseConfig::default(),
        tx,
    ));
    let store = Arc::new(InMemoryTxnStore::new());
    (Arc::new(TxnManager::new(store.clone(), lm)), store)
}

async fn spawn_slow(delay: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ep = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let stream = TcpListenerStream::new(listener);
    tokio::spawn(async move {
        Server::builder()
            .add_service(ParticipantServiceServer::new(SlowParticipant { delay }))
            .serve_with_incoming(stream)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    ep
}

/// Start a commit against a slow participant and wait until it is Voting.
async fn commit_in_flight(
    mgr: &Arc<TxnManager>,
    txn_id: &str,
) -> tokio::task::JoinHandle<Result<(), Error>> {
    let ep = spawn_slow(Duration::from_millis(600)).await;
    mgr.enlist(txn_id, ep, 0).await.unwrap();
    let m = mgr.clone();
    let id = txn_id.to_string();
    let h = tokio::spawn(async move { m.commit(&id).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        mgr.get_state(txn_id).await.unwrap(),
        TransactionState::Voting
    );
    h
}

#[tokio::test]
async fn abort_expired_during_voting_does_not_abort() {
    let (mgr, _) = make_manager();
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    let h = commit_in_flight(&mgr, &txn_id).await;

    mgr.abort_expired(&txn_id).await.unwrap();
    assert_eq!(
        mgr.get_state(&txn_id).await.unwrap(),
        TransactionState::Voting
    );

    h.await.unwrap().unwrap();
    assert_eq!(
        mgr.get_state(&txn_id).await.unwrap(),
        TransactionState::Committed
    );
}

#[tokio::test]
async fn second_commit_while_voting_is_in_progress_error() {
    let (mgr, _) = make_manager();
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    let h = commit_in_flight(&mgr, &txn_id).await;

    assert!(matches!(
        mgr.commit(&txn_id).await,
        Err(Error::TransactionCommitInProgress(_))
    ));
    assert!(matches!(
        mgr.abort(&txn_id).await,
        Err(Error::TransactionCommitInProgress(_))
    ));

    h.await.unwrap().unwrap();
    // Now terminal: commit is idempotent, abort is refused.
    mgr.commit(&txn_id).await.unwrap();
    assert!(matches!(
        mgr.abort(&txn_id).await,
        Err(Error::TransactionTerminal(_))
    ));
}

#[tokio::test]
async fn enlist_after_commit_cas_is_rejected() {
    let (mgr, store) = make_manager();
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    let h = commit_in_flight(&mgr, &txn_id).await;

    let err = mgr
        .enlist(&txn_id, "127.0.0.1:9".into(), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::TransactionTerminal(_)));
    assert_eq!(
        store
            .get(&txn_id)
            .await
            .unwrap()
            .unwrap()
            .participants
            .len(),
        1
    );

    h.await.unwrap().unwrap();
}

#[tokio::test]
async fn single_participant_transport_error_is_not_aborted() {
    let (mgr, _) = make_manager();
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    // Nothing listens here: connect fails -> transport error.
    mgr.enlist(&txn_id, "127.0.0.1:1".into(), 0).await.unwrap();

    let err = mgr.commit(&txn_id).await.unwrap_err();
    assert!(matches!(err, Error::TransactionOutcomeUnknown(_)));
    assert_eq!(
        mgr.get_state(&txn_id).await.unwrap(),
        TransactionState::Voting
    );
    // Lease expiry must not flip it to Aborted either.
    mgr.abort_expired(&txn_id).await.unwrap();
    assert_eq!(
        mgr.get_state(&txn_id).await.unwrap(),
        TransactionState::Voting
    );
}

#[tokio::test]
async fn store_cas_false_on_wrong_expected_state() {
    let (mgr, store) = make_manager();
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    assert!(!store
        .update_state_if(
            &txn_id,
            TransactionState::Voting,
            TransactionState::Committed
        )
        .await
        .unwrap());
    assert_eq!(
        mgr.get_state(&txn_id).await.unwrap(),
        TransactionState::Active
    );
}
