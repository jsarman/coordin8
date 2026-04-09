/// TransactionMgr demo — runs in-process, real gRPC participant servers.
///
/// Each test spins up one or more mock ParticipantService gRPC servers on
/// random ports, then drives TxnManager through the full 2PC lifecycle.
///
/// The key thing to watch: after Commit, participants' `committed` flag is true.
/// After Abort (for any reason), `committed` is false — no state change.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

use coordin8_core::{Leasing, TransactionState};
use coordin8_lease::LeaseManager;
use coordin8_proto::coordin8::{
    participant_service_server::{ParticipantService, ParticipantServiceServer},
    ParticipantRequest, PrepareResponse,
};
use coordin8_provider_local::{InMemoryLeaseStore, InMemoryTxnStore};
use coordin8_txn::TxnManager;

// ── Mock participant ──────────────────────────────────────────────────────────

/// A mock participant that records what happened to it.
struct MockParticipant {
    name: String,
    /// What vote to cast in Prepare (0=PREPARED, 1=NOTCHANGED, 2=ABORTED)
    prepare_vote: i32,
    pub prepared: Arc<AtomicBool>,
    pub committed: Arc<AtomicBool>,
    pub aborted: Arc<AtomicBool>,
}

impl MockParticipant {
    fn new(name: &str, prepare_vote: i32) -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
        let committed = Arc::new(AtomicBool::new(false));
        let aborted = Arc::new(AtomicBool::new(false));
        let p = Self {
            name: name.to_string(),
            prepare_vote,
            prepared: Arc::new(AtomicBool::new(false)),
            committed: Arc::clone(&committed),
            aborted: Arc::clone(&aborted),
        };
        (p, committed, aborted)
    }
}

#[tonic::async_trait]
impl ParticipantService for MockParticipant {
    async fn prepare(
        &self,
        req: Request<ParticipantRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        self.prepared.store(true, Ordering::SeqCst);
        let vote_name = match self.prepare_vote {
            1 => "NOTCHANGED",
            2 => "ABORTED",
            _ => "PREPARED",
        };
        println!(
            "    [{}] prepare({}) → {}",
            self.name,
            &req.into_inner().txn_id[..8],
            vote_name
        );
        Ok(Response::new(PrepareResponse {
            vote: self.prepare_vote,
        }))
    }

    async fn commit(&self, req: Request<ParticipantRequest>) -> Result<Response<()>, Status> {
        self.committed.store(true, Ordering::SeqCst);
        println!(
            "    [{}] commit({})",
            self.name,
            &req.into_inner().txn_id[..8]
        );
        Ok(Response::new(()))
    }

    async fn abort(&self, req: Request<ParticipantRequest>) -> Result<Response<()>, Status> {
        self.aborted.store(true, Ordering::SeqCst);
        println!(
            "    [{}] abort({})",
            self.name,
            &req.into_inner().txn_id[..8]
        );
        Ok(Response::new(()))
    }

    async fn prepare_and_commit(
        &self,
        req: Request<ParticipantRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        let tid = &req.into_inner().txn_id[..8].to_string();
        if self.prepare_vote == 0 {
            self.committed.store(true, Ordering::SeqCst);
            println!(
                "    [{}] prepare_and_commit({}) → PREPARED+committed",
                self.name, tid
            );
        } else {
            self.aborted.store(true, Ordering::SeqCst);
            println!("    [{}] prepare_and_commit({}) → ABORTED", self.name, tid);
        }
        Ok(Response::new(PrepareResponse {
            vote: self.prepare_vote,
        }))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_manager() -> Arc<TxnManager> {
    let lease_store = Arc::new(InMemoryLeaseStore::new());
    let lease_manager: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
        lease_store,
        coordin8_core::LeaseConfig::default(),
    ));
    let txn_store = Arc::new(InMemoryTxnStore::new());
    Arc::new(TxnManager::new(txn_store, lease_manager))
}

/// Spin up a mock participant on a random port. Returns (endpoint, committed, aborted).
async fn spawn_participant(
    name: &str,
    prepare_vote: i32,
) -> (String, Arc<AtomicBool>, Arc<AtomicBool>) {
    let (participant, committed, aborted) = MockParticipant::new(name, prepare_vote);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint = format!("127.0.0.1:{}", port);
    let stream = TcpListenerStream::new(listener);

    tokio::spawn(async move {
        Server::builder()
            .add_service(ParticipantServiceServer::new(participant))
            .serve_with_incoming(stream)
            .await
            .ok();
    });

    // Let the server start
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    (endpoint, committed, aborted)
}

// ── Test 1: Single participant — happy path (PrepareAndCommit optimization) ───

#[tokio::test]
async fn single_participant_commit() {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Test 1: Single participant — happy path");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let mgr = make_manager();
    let (ep, committed, aborted) = spawn_participant("store-A", 0 /* PREPARED */).await;

    let (txn_id, _lease) = mgr.begin(60).await.unwrap();
    println!("  Begin:  txn={}", &txn_id[..8]);

    mgr.enlist(&txn_id, ep, 0).await.unwrap();
    println!("  Enlist: store-A enlisted");

    println!("  Commit: running 2PC (single-participant → PrepareAndCommit)...");
    mgr.commit(&txn_id).await.unwrap();

    let state = mgr.get_state(&txn_id).await.unwrap();
    println!("  State:  {:?}", state);
    println!(
        "  store-A committed={} aborted={}",
        committed.load(Ordering::SeqCst),
        aborted.load(Ordering::SeqCst)
    );

    assert_eq!(state, TransactionState::Committed);
    assert!(
        committed.load(Ordering::SeqCst),
        "store-A should have committed"
    );
    assert!(
        !aborted.load(Ordering::SeqCst),
        "store-A should NOT have aborted"
    );
    println!("  ✓ State is COMMITTED. Participant committed. No abort.");
}

// ── Test 2: Two participants — both vote PREPARED → commit ────────────────────

#[tokio::test]
async fn multi_participant_all_prepared_commits() {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Test 2: Two participants — both vote PREPARED");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let mgr = make_manager();
    let (ep_a, committed_a, aborted_a) = spawn_participant("store-A", 0 /* PREPARED */).await;
    let (ep_b, committed_b, aborted_b) = spawn_participant("store-B", 0 /* PREPARED */).await;

    let (txn_id, _) = mgr.begin(60).await.unwrap();
    println!("  Begin:  txn={}", &txn_id[..8]);

    mgr.enlist(&txn_id, ep_a, 0).await.unwrap();
    mgr.enlist(&txn_id, ep_b, 0).await.unwrap();
    println!("  Enlist: store-A, store-B");

    println!("  Commit: running 2PC phase 1 (prepare) → phase 2 (commit)...");
    mgr.commit(&txn_id).await.unwrap();

    let state = mgr.get_state(&txn_id).await.unwrap();
    println!("  State:  {:?}", state);
    println!(
        "  store-A committed={} aborted={}",
        committed_a.load(Ordering::SeqCst),
        aborted_a.load(Ordering::SeqCst)
    );
    println!(
        "  store-B committed={} aborted={}",
        committed_b.load(Ordering::SeqCst),
        aborted_b.load(Ordering::SeqCst)
    );

    assert_eq!(state, TransactionState::Committed);
    assert!(committed_a.load(Ordering::SeqCst));
    assert!(committed_b.load(Ordering::SeqCst));
    assert!(!aborted_a.load(Ordering::SeqCst));
    assert!(!aborted_b.load(Ordering::SeqCst));
    println!("  ✓ Both committed. No aborts.");
}

// ── Test 3: One participant votes ABORTED → everyone aborts, nothing committed

#[tokio::test]
async fn participant_vetoes_aborts_everyone() {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Test 3: One participant vetoes — NO state change");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let mgr = make_manager();
    let (ep_a, committed_a, aborted_a) = spawn_participant("store-A", 0 /* PREPARED */).await;
    let (ep_b, committed_b, aborted_b) = spawn_participant("store-B", 2 /* ABORTED  */).await;

    let (txn_id, _) = mgr.begin(60).await.unwrap();
    println!("  Begin:  txn={}", &txn_id[..8]);

    mgr.enlist(&txn_id, ep_a, 0).await.unwrap();
    mgr.enlist(&txn_id, ep_b, 0).await.unwrap();
    println!("  Enlist: store-A (PREPARED), store-B (will vote ABORTED)");

    println!("  Commit: running 2PC... store-B will veto...");
    let result = mgr.commit(&txn_id).await;

    let state = mgr.get_state(&txn_id).await.unwrap();
    println!("  State:  {:?}", state);
    println!(
        "  store-A committed={} aborted={}",
        committed_a.load(Ordering::SeqCst),
        aborted_a.load(Ordering::SeqCst)
    );
    println!(
        "  store-B committed={} aborted={}",
        committed_b.load(Ordering::SeqCst),
        aborted_b.load(Ordering::SeqCst)
    );

    assert!(
        result.is_err(),
        "commit should return error when a participant vetoes"
    );
    assert_eq!(state, TransactionState::Aborted);
    assert!(
        !committed_a.load(Ordering::SeqCst),
        "store-A must NOT have committed — veto means nothing commits"
    );
    assert!(
        !committed_b.load(Ordering::SeqCst),
        "store-B must NOT have committed"
    );
    assert!(
        aborted_a.load(Ordering::SeqCst),
        "store-A should have received abort"
    );
    assert!(
        aborted_b.load(Ordering::SeqCst),
        "store-B should have received abort"
    );
    println!("  ✓ State is ABORTED. Neither participant committed. This is the guarantee.");
}

// ── Test 4: Explicit abort before any commit — nothing committed ──────────────

#[tokio::test]
async fn explicit_abort_no_state_change() {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Test 4: Explicit abort — no state change");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let mgr = make_manager();
    let (ep_a, committed_a, aborted_a) = spawn_participant("store-A", 0 /* PREPARED */).await;
    let (ep_b, committed_b, aborted_b) = spawn_participant("store-B", 0 /* PREPARED */).await;

    let (txn_id, _) = mgr.begin(60).await.unwrap();
    println!("  Begin:  txn={}", &txn_id[..8]);

    mgr.enlist(&txn_id, ep_a, 0).await.unwrap();
    mgr.enlist(&txn_id, ep_b, 0).await.unwrap();
    println!("  Enlist: store-A, store-B");

    println!("  Abort:  caller decides to abort (no prepare phase runs)...");
    mgr.abort(&txn_id).await.unwrap();

    let state = mgr.get_state(&txn_id).await.unwrap();
    println!("  State:  {:?}", state);
    println!(
        "  store-A committed={} aborted={}",
        committed_a.load(Ordering::SeqCst),
        aborted_a.load(Ordering::SeqCst)
    );
    println!(
        "  store-B committed={} aborted={}",
        committed_b.load(Ordering::SeqCst),
        aborted_b.load(Ordering::SeqCst)
    );

    assert_eq!(state, TransactionState::Aborted);
    assert!(
        !committed_a.load(Ordering::SeqCst),
        "store-A must NOT have committed"
    );
    assert!(
        !committed_b.load(Ordering::SeqCst),
        "store-B must NOT have committed"
    );
    assert!(
        aborted_a.load(Ordering::SeqCst),
        "store-A should have received abort"
    );
    assert!(
        aborted_b.load(Ordering::SeqCst),
        "store-B should have received abort"
    );
    println!("  ✓ State is ABORTED. Neither participant committed. Clean rollback.");
}

// ── Test 5: NOTCHANGED voter excluded from commit phase ───────────────────────

#[tokio::test]
async fn notchanged_voter_skipped_at_commit() {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Test 5: NOTCHANGED voter — read-only participant skipped at commit");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let mgr = make_manager();
    let (ep_a, committed_a, _) = spawn_participant("writer", 0 /* PREPARED    */).await;
    let (ep_b, committed_b, _) = spawn_participant("readonly", 1 /* NOTCHANGED  */).await;

    let (txn_id, _) = mgr.begin(60).await.unwrap();
    println!("  Begin:  txn={}", &txn_id[..8]);

    mgr.enlist(&txn_id, ep_a, 0).await.unwrap();
    mgr.enlist(&txn_id, ep_b, 0).await.unwrap();
    println!("  Enlist: writer (PREPARED), readonly (NOTCHANGED)");

    println!("  Commit: writer will be committed, readonly will be skipped...");
    mgr.commit(&txn_id).await.unwrap();

    let state = mgr.get_state(&txn_id).await.unwrap();
    println!("  State:  {:?}", state);
    println!(
        "  writer   committed={}",
        committed_a.load(Ordering::SeqCst)
    );
    println!(
        "  readonly committed={} (should be false — no commit call needed)",
        committed_b.load(Ordering::SeqCst)
    );

    assert_eq!(state, TransactionState::Committed);
    assert!(
        committed_a.load(Ordering::SeqCst),
        "writer should have committed"
    );
    assert!(
        !committed_b.load(Ordering::SeqCst),
        "readonly should NOT receive commit call"
    );
    println!("  ✓ COMMITTED. Writer got commit. Read-only participant skipped.");
}

// ── Test 6: Lease expiry auto-aborts transaction ──────────────────────────────

#[tokio::test]
async fn lease_expiry_auto_aborts() {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Test 6: Lease expiry → auto-abort");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let mgr = make_manager();
    let (ep_a, committed_a, aborted_a) = spawn_participant("store-A", 0 /* PREPARED */).await;

    let (txn_id, _lease) = mgr.begin(60).await.unwrap();
    println!("  Begin:  txn={}", &txn_id[..8]);

    mgr.enlist(&txn_id, ep_a, 0).await.unwrap();
    println!("  Enlist: store-A enlisted");

    // Simulate lease expiry cascade
    println!("  [cascade] Lease expired — calling abort_expired...");
    mgr.abort_expired(&txn_id).await.unwrap();

    let state = mgr.get_state(&txn_id).await.unwrap();
    println!("  State:  {:?}", state);
    println!(
        "  store-A committed={} aborted={}",
        committed_a.load(Ordering::SeqCst),
        aborted_a.load(Ordering::SeqCst)
    );

    assert_eq!(state, TransactionState::Aborted);
    assert!(
        !committed_a.load(Ordering::SeqCst),
        "store-A must NOT have committed"
    );
    assert!(
        aborted_a.load(Ordering::SeqCst),
        "store-A should have received abort"
    );
    println!("  ✓ Lease expired → ABORTED. Participant got abort call. No commits.");
}

// ── Test 7: Zero participants — trivial commit ────────────────────────────────

#[tokio::test]
async fn zero_participants_trivial_commit() {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Test 6: Zero participants — trivial commit");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let mgr = make_manager();
    let (txn_id, _) = mgr.begin(60).await.unwrap();
    println!("  Begin:  txn={}", &txn_id[..8]);
    println!("  (no participants enlisted)");

    mgr.commit(&txn_id).await.unwrap();
    let state = mgr.get_state(&txn_id).await.unwrap();
    println!("  State:  {:?}", state);

    assert_eq!(state, TransactionState::Committed);
    println!("  ✓ COMMITTED trivially. No participants = nothing to coordinate.");
}
