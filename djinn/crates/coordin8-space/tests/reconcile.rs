//! The participant's half of 2PC recovery (N1, review 2026-11-14): Space
//! finishes a transaction whose Commit/Abort never reached it by asking the
//! coordinator for the decision.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use coordin8_core::{Error, Leasing, TransactionState, TupleRecord, TxnEnlister};
use coordin8_lease::LeaseManager;
use coordin8_provider_local::{InMemoryLeaseStore, InMemorySpaceStore};
use coordin8_space::SpaceManager;
use tokio::sync::broadcast;

/// A coordinator whose recorded states the test sets directly.
#[derive(Default)]
struct FakeCoordinator {
    states: Mutex<HashMap<String, TransactionState>>,
    unreachable: Mutex<bool>,
}

impl FakeCoordinator {
    fn set(&self, txn: &str, s: TransactionState) {
        self.states.lock().unwrap().insert(txn.into(), s);
    }
}

#[async_trait::async_trait]
impl TxnEnlister for FakeCoordinator {
    async fn enlist(&self, _: &str, _: &str) -> Result<(), Error> {
        Ok(())
    }
    async fn state(&self, txn_id: &str) -> Result<Option<TransactionState>, Error> {
        if *self.unreachable.lock().unwrap() {
            return Err(Error::Unavailable("down".into()));
        }
        Ok(self.states.lock().unwrap().get(txn_id).cloned())
    }
}

fn make() -> (Arc<SpaceManager>, Arc<FakeCoordinator>) {
    let (lease_tx, _) = broadcast::channel(256);
    let leases: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
        Arc::new(InMemoryLeaseStore::new()),
        coordin8_core::LeaseConfig::default(),
        lease_tx,
    ));
    let (tuple_tx, _) = broadcast::channel::<TupleRecord>(256);
    let (expiry_tx, _) = broadcast::channel::<TupleRecord>(256);
    let coord = Arc::new(FakeCoordinator::default());
    let mgr = SpaceManager::with_enlister(
        Arc::new(InMemorySpaceStore::new()),
        leases,
        tuple_tx,
        expiry_tx,
        coord.clone(),
        "127.0.0.1:1".into(),
    );
    (Arc::new(mgr), coord)
}

fn tmpl(kind: &str) -> HashMap<String, String> {
    HashMap::from([("kind".to_string(), kind.to_string())])
}

async fn write(mgr: &SpaceManager, kind: &str, txn: Option<&str>) {
    mgr.write(
        tmpl(kind),
        vec![],
        60,
        "test".into(),
        None,
        txn.map(String::from),
    )
    .await
    .unwrap();
}

async fn visible(mgr: &SpaceManager, kind: &str) -> bool {
    mgr.read(tmpl(kind), false, 0, None)
        .await
        .unwrap()
        .is_some()
}

/// The review's repro: a tuple taken under a txn the coordinator aborted,
/// with the Abort never delivered. Reconcile restores it.
#[tokio::test]
async fn aborted_txn_restores_taken_tuple() {
    let (mgr, coord) = make();
    write(&mgr, "job", None).await;
    assert!(mgr
        .take(tmpl("job"), false, 0, Some("t1".into()))
        .await
        .unwrap()
        .is_some());
    assert!(!visible(&mgr, "job").await);

    coord.set("t1", TransactionState::Active);
    assert_eq!(
        mgr.reconcile_txns().await.unwrap(),
        0,
        "undecided: left alone"
    );
    assert!(!visible(&mgr, "job").await);

    coord.set("t1", TransactionState::Aborted);
    assert_eq!(mgr.reconcile_txns().await.unwrap(), 1);
    assert!(visible(&mgr, "job").await, "taken tuple restored");
    assert!(!mgr.has_txn("t1").await.unwrap());
    assert_eq!(mgr.reconcile_txns().await.unwrap(), 0, "nothing left");
}

#[tokio::test]
async fn decided_commit_publishes_writes() {
    for decided in [TransactionState::Prepared, TransactionState::Committed] {
        let (mgr, coord) = make();
        write(&mgr, "out", Some("t2")).await;
        assert!(!visible(&mgr, "out").await);
        coord.set("t2", TransactionState::Voting);
        assert_eq!(mgr.reconcile_txns().await.unwrap(), 0);
        coord.set("t2", decided.clone());
        assert_eq!(mgr.reconcile_txns().await.unwrap(), 1, "{decided:?}");
        assert!(visible(&mgr, "out").await, "{decided:?}");
    }
}

/// No record at the coordinator (it never committed it and has forgotten
/// it, e.g. an in-memory TxnMgr restarted): abort.
#[tokio::test]
async fn unknown_txn_is_aborted_and_unreachable_coordinator_is_not() {
    let (mgr, coord) = make();
    write(&mgr, "w", Some("t3")).await;

    *coord.unreachable.lock().unwrap() = true;
    assert_eq!(mgr.reconcile_txns().await.unwrap(), 0);
    assert!(
        mgr.has_txn("t3").await.unwrap(),
        "no answer is not an answer"
    );

    *coord.unreachable.lock().unwrap() = false;
    assert_eq!(mgr.reconcile_txns().await.unwrap(), 1);
    assert!(!mgr.has_txn("t3").await.unwrap());
    assert!(!visible(&mgr, "w").await, "uncommitted write discarded");
}
