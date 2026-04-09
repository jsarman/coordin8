//! Integration tests for split-mode TransactionMgr.
//!
//! Boots Registry + LeaseMgr + TransactionMgr as three separate services.
//! TxnMgr runs against `RemoteLeasing`, so every `begin` grant and every
//! `abort`/`commit` lease cancel crosses a gRPC hop to the remote LeaseMgr.
//! The `txn:` expiry prefix is driven by `watch_expiry_prefix` so dead
//! transactions are auto-aborted even when the lease died remotely.

use std::time::Duration;

use tokio::task::JoinHandle;

use coordin8_djinn::services::{
    run_lease_on_listener, run_registry_on_listener, run_txn_on_listener,
};
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

async fn spawn_lease(registry_addr: &str, ttl: u64) -> JoinHandle<()> {
    let (listener, _) = ephemeral_listener().await;
    let registry_addr = registry_addr.to_string();
    let handle = tokio::spawn(async move {
        run_lease_on_listener(listener, Some(&registry_addr), "127.0.0.1", ttl)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle
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
    tokio::time::sleep(Duration::from_millis(500)).await;
    (handle, addr)
}

/// Happy path: begin a zero-participant transaction, commit it, confirm the
/// state transitions through the split-mode wire. Every lease grant/cancel
/// crosses `RemoteLeasing`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_txn_begin_and_commit_round_trip() {
    let (_reg, registry_addr) = spawn_registry().await;
    let _lease = spawn_lease(&registry_addr, 30).await;
    let (_txn, txn_addr) = spawn_txn(&registry_addr).await;

    let mut client = TransactionServiceClient::connect(txn_addr)
        .await
        .expect("dial split-mode TxnMgr");

    let created = client
        .begin(BeginRequest { ttl_seconds: 30 })
        .await
        .expect("begin (proves RemoteLeasing.grant worked)")
        .into_inner();

    assert!(!created.txn_id.is_empty());
    assert!(created.lease.is_some(), "begin should return a lease");

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

/// A short-TTL transaction whose lease expires on the remote LeaseMgr must
/// be auto-aborted on the split TxnMgr. This exercises the
/// `watch_expiry_prefix("txn:")` task in `run_txn_on_listener` — without it,
/// `abort_expired` never fires and the txn stays Active forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_txn_remote_expiry_auto_aborts() {
    let (_reg, registry_addr) = spawn_registry().await;
    let _lease = spawn_lease(&registry_addr, 30).await;
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

    // Wait for: TTL (2s) + reaper tick (≤1s) + WatchExpiry delivery +
    // abort_expired to run on the split TxnMgr.
    tokio::time::sleep(Duration::from_secs(5)).await;

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
        "txn should have been auto-aborted via watch_expiry_prefix path"
    );
}
