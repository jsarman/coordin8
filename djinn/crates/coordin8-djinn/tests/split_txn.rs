//! Integration tests for split-mode TransactionMgr.
//!
//! Boots Registry + TransactionMgr as two separate services. TxnMgr embeds
//! its own `LeaseManager` for transaction leases — no external LeaseMgr
//! dependency at all; Registry is only used for (optional) self-registration.

use std::time::Duration;

use tokio::task::JoinHandle;

use coordin8_djinn::services::{run_registry_on_listener, run_txn_on_listener};
use coordin8_proto::coordin8::{
    transaction_service_client::TransactionServiceClient, BeginRequest, CommitRequest,
    GetStateRequest, TransactionState,
};

async fn ephemeral_listener() -> (tokio::net::TcpListener, u16) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}

async fn spawn_registry() -> (JoinHandle<()>, String) {
    let (listener, port) = ephemeral_listener().await;
    let addr = format!("http://127.0.0.1:{port}");
    let handle = tokio::spawn(async move {
        run_registry_on_listener(listener).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (handle, addr)
}

async fn spawn_txn(registry_addr: &str) -> (JoinHandle<()>, String) {
    let (listener, port) = ephemeral_listener().await;
    let addr = format!("http://127.0.0.1:{port}");
    let registry_addr = registry_addr.to_string();
    let handle = tokio::spawn(async move {
        run_txn_on_listener(listener, &registry_addr, "127.0.0.1", 30)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (handle, addr)
}

/// Happy path: begin a zero-participant transaction, commit it, confirm the
/// state transitions through the split-mode wire. Every lease grant/cancel
/// is handled by TxnMgr's own embedded LeaseManager.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_txn_begin_and_commit_round_trip() {
    let (_reg, registry_addr) = spawn_registry().await;
    let (_txn, txn_addr) = spawn_txn(&registry_addr).await;

    let mut client = TransactionServiceClient::connect(txn_addr)
        .await
        .expect("dial split-mode TxnMgr");

    let created = client
        .begin(BeginRequest { ttl_seconds: 30 })
        .await
        .expect("begin (proves the embedded LeaseManager grant worked)")
        .into_inner();

    assert!(!created.txn_id.is_empty());
    let lease = created.lease.expect("begin should return a lease");
    assert!(
        !lease.grantor_host.is_empty() && lease.grantor_port != 0,
        "lease should carry a grantor address to renew against"
    );

    client
        .commit(CommitRequest {
            txn_id: created.txn_id.clone(),
            wait_millis: 0,
        })
        .await
        .expect("commit zero-participant txn");

    let state_resp = client
        .get_state(GetStateRequest {
            txn_id: created.txn_id,
        })
        .await
        .expect("get_state after commit")
        .into_inner();

    assert_eq!(
        state_resp.state,
        TransactionState::Committed as i32,
        "txn should be in Committed state after commit"
    );
}

/// A short-TTL transaction whose lease expires must be auto-aborted on the
/// split TxnMgr. This exercises the cascade wired in `run_txn_on_listener` —
/// without it, `abort_expired` never fires and the txn stays Active forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_txn_expiry_auto_aborts() {
    let (_reg, registry_addr) = spawn_registry().await;
    let (_txn, txn_addr) = spawn_txn(&registry_addr).await;

    let mut client = TransactionServiceClient::connect(txn_addr)
        .await
        .expect("dial split-mode TxnMgr");

    let created = client
        .begin(BeginRequest { ttl_seconds: 2 })
        .await
        .expect("begin short-TTL txn")
        .into_inner();

    // Sanity: Active right after begin.
    let initial = client
        .get_state(GetStateRequest {
            txn_id: created.txn_id.clone(),
        })
        .await
        .expect("get_state initial")
        .into_inner();
    assert_eq!(initial.state, TransactionState::Active as i32);

    // Wait for: TTL (2s) + reaper tick (≤1s) + cascade to run.
    tokio::time::sleep(Duration::from_secs(4)).await;

    let after = client
        .get_state(GetStateRequest {
            txn_id: created.txn_id,
        })
        .await
        .expect("get_state after expiry")
        .into_inner();

    assert_eq!(
        after.state,
        TransactionState::Aborted as i32,
        "txn should have been auto-aborted via the embedded LeaseManager cascade"
    );
}
