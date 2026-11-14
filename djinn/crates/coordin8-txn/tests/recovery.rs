//! Transaction recovery: nothing is left in `Voting`/`Prepared` for good.
//! A failed phase-2 call, a lost single-participant reply and a coordinator
//! crash mid-vote each end in a final state once `recover()` runs (or the
//! client retries `commit()`).
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

// Wire votes (see transaction.proto); spelled out so this file doesn't care
// which numbering the enum uses.
fn prepared() -> i32 {
    coordin8_proto::coordin8::PrepareVote::from_str_name("VOTE_PREPARED").unwrap() as i32
}
fn notchanged() -> i32 {
    coordin8_proto::coordin8::PrepareVote::from_str_name("VOTE_NOTCHANGED").unwrap() as i32
}

/// A participant whose first `fail_*` calls of a kind fail after the
/// connection is up (so the coordinator can't know whether they applied).
/// PrepareAndCommit is idempotent: once committed it answers NOTCHANGED.
#[derive(Default)]
struct Flaky {
    fail_commit: AtomicUsize,
    fail_pac: AtomicUsize,
    commits: AtomicUsize,
    aborts: AtomicUsize,
    committed: AtomicBool,
}

fn take_failure(n: &AtomicUsize) -> bool {
    n.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
        .is_ok()
}

struct Svc(Arc<Flaky>);

#[tonic::async_trait]
impl ParticipantService for Svc {
    async fn prepare(
        &self,
        _: Request<ParticipantRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        Ok(Response::new(PrepareResponse { vote: prepared() }))
    }
    async fn commit(&self, _: Request<ParticipantRequest>) -> Result<Response<()>, Status> {
        if take_failure(&self.0.fail_commit) {
            return Err(Status::internal("injected commit failure"));
        }
        self.0.commits.fetch_add(1, Ordering::SeqCst);
        self.0.committed.store(true, Ordering::SeqCst);
        Ok(Response::new(()))
    }
    async fn abort(&self, _: Request<ParticipantRequest>) -> Result<Response<()>, Status> {
        self.0.aborts.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(()))
    }
    async fn prepare_and_commit(
        &self,
        _: Request<ParticipantRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        let was = self.0.committed.swap(true, Ordering::SeqCst);
        if !was {
            self.0.commits.fetch_add(1, Ordering::SeqCst);
        }
        // Applied, then the reply is "lost".
        if take_failure(&self.0.fail_pac) {
            return Err(Status::internal("injected lost reply"));
        }
        let vote = if was { notchanged() } else { prepared() };
        Ok(Response::new(PrepareResponse { vote }))
    }
}

async fn spawn(p: Arc<Flaky>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ep = listener.local_addr().unwrap().to_string();
    tokio::spawn(
        Server::builder()
            .add_service(ParticipantServiceServer::new(Svc(p)))
            .serve_with_incoming(TcpListenerStream::new(listener)),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    ep
}

fn make_manager() -> (Arc<TxnManager>, Arc<InMemoryTxnStore>) {
    let (tx, _) = tokio::sync::broadcast::channel(256);
    let lm: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
        Arc::new(InMemoryLeaseStore::new()),
        coordin8_core::LeaseConfig::default(),
        tx,
    ));
    let store = Arc::new(InMemoryTxnStore::new());
    (Arc::new(TxnManager::new(store.clone(), lm)), store)
}

async fn state(store: &InMemoryTxnStore, id: &str) -> TransactionState {
    store.get(id).await.unwrap().unwrap().state
}

/// A participant that misses its phase-2 Commit leaves the decision
/// recorded as `Prepared`; recovery re-sends Commit until it acks.
#[tokio::test]
async fn failed_phase_two_commit_is_redriven() {
    let (mgr, store) = make_manager();
    let ok = Arc::new(Flaky::default());
    let flaky = Arc::new(Flaky::default());
    flaky.fail_commit.store(2, Ordering::SeqCst);
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    mgr.enlist(&txn_id, spawn(ok.clone()).await, 0)
        .await
        .unwrap();
    mgr.enlist(&txn_id, spawn(flaky.clone()).await, 0)
        .await
        .unwrap();

    // The decision is commit, so the caller is told so.
    mgr.commit(&txn_id).await.unwrap();
    assert_eq!(state(&store, &txn_id).await, TransactionState::Prepared);
    assert_eq!(flaky.commits.load(Ordering::SeqCst), 0);

    // First sweep: still failing. Second: acked, and the txn is final.
    assert_eq!(mgr.recover().await.unwrap(), 0);
    assert_eq!(state(&store, &txn_id).await, TransactionState::Prepared);
    assert_eq!(mgr.recover().await.unwrap(), 1);
    assert_eq!(state(&store, &txn_id).await, TransactionState::Committed);
    assert_eq!(flaky.commits.load(Ordering::SeqCst), 1);
    assert_eq!(mgr.recover().await.unwrap(), 0, "nothing left to recover");
}

/// A client retrying commit() on a stuck `Prepared` txn finishes it rather
/// than getting CommitInProgress forever.
#[tokio::test]
async fn commit_retry_finishes_prepared() {
    let (mgr, store) = make_manager();
    let a = Arc::new(Flaky::default());
    let b = Arc::new(Flaky::default());
    b.fail_commit.store(1, Ordering::SeqCst);
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    mgr.enlist(&txn_id, spawn(a).await, 0).await.unwrap();
    mgr.enlist(&txn_id, spawn(b.clone()).await, 0)
        .await
        .unwrap();
    mgr.commit(&txn_id).await.unwrap();
    assert_eq!(state(&store, &txn_id).await, TransactionState::Prepared);

    mgr.commit(&txn_id).await.unwrap();
    assert_eq!(state(&store, &txn_id).await, TransactionState::Committed);
    assert_eq!(b.commits.load(Ordering::SeqCst), 1);
}

/// Single participant, reply lost after it applied: outcome unknown, then
/// recovery asks again (PrepareAndCommit is idempotent) and records Committed.
#[tokio::test]
async fn lost_single_participant_reply_is_resolved() {
    let (mgr, store) = make_manager();
    let p = Arc::new(Flaky::default());
    p.fail_pac.store(1, Ordering::SeqCst);
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    mgr.enlist(&txn_id, spawn(p.clone()).await, 0)
        .await
        .unwrap();

    assert!(matches!(
        mgr.commit(&txn_id).await,
        Err(Error::TransactionOutcomeUnknown(_))
    ));
    assert_eq!(state(&store, &txn_id).await, TransactionState::Voting);

    assert_eq!(mgr.recover().await.unwrap(), 1);
    assert_eq!(state(&store, &txn_id).await, TransactionState::Committed);
    assert_eq!(p.commits.load(Ordering::SeqCst), 1, "applied exactly once");
}

/// Single participant that was never reachable: nothing was sent, so the
/// commit aborts on the spot instead of hanging in Voting.
#[tokio::test]
async fn unreachable_single_participant_aborts() {
    let (mgr, store) = make_manager();
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    // Nothing listens here: connection refused.
    mgr.enlist(&txn_id, "127.0.0.1:1".into(), 0).await.unwrap();

    assert!(matches!(
        mgr.commit(&txn_id).await,
        Err(Error::TransactionAborted(_))
    ));
    assert_eq!(state(&store, &txn_id).await, TransactionState::Aborted);
    assert!(matches!(
        mgr.commit(&txn_id).await,
        Err(Error::TransactionAborted(_))
    ));
}

/// Coordinator died mid-vote with several participants: no decision was
/// recorded, so recovery aborts everywhere.
#[tokio::test]
async fn voting_without_decision_is_aborted() {
    let (mgr, store) = make_manager();
    let a = Arc::new(Flaky::default());
    let b = Arc::new(Flaky::default());
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    mgr.enlist(&txn_id, spawn(a.clone()).await, 0)
        .await
        .unwrap();
    mgr.enlist(&txn_id, spawn(b.clone()).await, 0)
        .await
        .unwrap();
    // The crash: Voting recorded, nothing after it.
    assert!(store
        .update_state_if(&txn_id, TransactionState::Active, TransactionState::Voting)
        .await
        .unwrap());

    assert_eq!(mgr.recover().await.unwrap(), 1);
    assert_eq!(state(&store, &txn_id).await, TransactionState::Aborted);
    assert_eq!(a.aborts.load(Ordering::SeqCst), 1);
    assert_eq!(b.aborts.load(Ordering::SeqCst), 1);
    assert_eq!(a.commits.load(Ordering::SeqCst), 0);
}

/// Recovery never touches a commit that is live in this process.
#[tokio::test]
async fn recovery_skips_live_commit() {
    struct Slow;
    #[tonic::async_trait]
    impl ParticipantService for Slow {
        async fn prepare(
            &self,
            _: Request<ParticipantRequest>,
        ) -> Result<Response<PrepareResponse>, Status> {
            tokio::time::sleep(Duration::from_millis(400)).await;
            Ok(Response::new(PrepareResponse { vote: prepared() }))
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
            Ok(Response::new(PrepareResponse { vote: prepared() }))
        }
    }
    let (mgr, store) = make_manager();
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    for _ in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ep = listener.local_addr().unwrap().to_string();
        tokio::spawn(
            Server::builder()
                .add_service(ParticipantServiceServer::new(Slow))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        mgr.enlist(&txn_id, ep, 0).await.unwrap();
    }
    let (m, id) = (mgr.clone(), txn_id.clone());
    let h = tokio::spawn(async move { m.commit(&id).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(state(&store, &txn_id).await, TransactionState::Voting);

    assert_eq!(mgr.recover().await.unwrap(), 0);
    h.await.unwrap().unwrap();
    assert_eq!(state(&store, &txn_id).await, TransactionState::Committed);
}
