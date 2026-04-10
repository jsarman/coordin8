//! Integration tests for Space auto-enlist + 2PC in split mode.
//!
//! Boots Registry + LeaseMgr + TransactionMgr + Space as four separate
//! services. A transactional `write` on Space must cause Space to enlist
//! with TxnMgr as a 2PC participant; on commit, TxnMgr must drive Prepare
//! and Commit back into Space's `ParticipantService`, flushing the tuple
//! from the uncommitted buffer into the visible store.
//!
//! These tests exercise the full wire path: `RemoteTxnEnlister` (Space →
//! TxnMgr), Registry-based TxnMgr discovery, and the callback loop from
//! TxnMgr back into Space's participant endpoint.

use std::collections::HashMap;
use std::time::Duration;

use tokio::task::JoinHandle;

use coordin8_djinn::services::{
    run_lease_on_listener, run_registry_on_listener, run_space_on_listener, run_txn_on_listener,
};
use coordin8_proto::coordin8::{
    space_service_client::SpaceServiceClient, transaction_service_client::TransactionServiceClient,
    AbortRequest, BeginRequest, CommitRequest, ReadRequest, WriteRequest,
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

async fn spawn_space(registry_addr: &str) -> (JoinHandle<()>, String) {
    let (listener, port) = ephemeral_listener().await;
    let addr = format!("http://127.0.0.1:{port}");
    let registry_addr = registry_addr.to_string();
    let handle = tokio::spawn(async move {
        run_space_on_listener(listener, &registry_addr, "127.0.0.1", 30)
            .await
            .ok();
    });
    // Space discovery of TxnMgr also happens during boot — give it a beat.
    tokio::time::sleep(Duration::from_millis(800)).await;
    (handle, addr)
}

/// Boot order matters only for discovery: Space calls `discover_txn_mgr` at
/// boot, so TxnMgr must be up first. Registry and LeaseMgr are always the
/// bedrock.
async fn boot_stack() -> (String, String) {
    let (_reg, registry_addr) = spawn_registry().await;
    let _lease = spawn_lease(&registry_addr, 30).await;
    let (_txn, txn_addr) = spawn_txn(&registry_addr).await;
    let (_space, space_addr) = spawn_space(&registry_addr).await;
    // Leak the JoinHandles intentionally — tests run to completion and the
    // process exits. Holding owners alive keeps rustc from dropping the
    // spawned tasks early.
    std::mem::forget(_reg);
    std::mem::forget(_lease);
    std::mem::forget(_txn);
    std::mem::forget(_space);
    (txn_addr, space_addr)
}

/// A transactional write is invisible to non-transactional reads until the
/// txn commits. After commit, TxnMgr drives Prepare+Commit back into Space
/// via the auto-enlisted participant endpoint, and the tuple becomes visible.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn split_space_txn_auto_enlist_and_commit() {
    let (txn_addr, space_addr) = boot_stack().await;

    let mut space = SpaceServiceClient::connect(space_addr)
        .await
        .expect("dial Space");
    let mut txn = TransactionServiceClient::connect(txn_addr)
        .await
        .expect("dial TxnMgr");

    // Begin a transaction.
    let created = txn
        .begin(BeginRequest { ttl_seconds: 30 })
        .await
        .expect("begin txn")
        .into_inner();
    let txn_id = created.txn_id;
    assert!(!txn_id.is_empty());

    // Transactional write — this must trigger Space → TxnMgr auto-enlist.
    let mut attrs = HashMap::new();
    attrs.insert("ticker".to_string(), "NVDA".to_string());
    space
        .write(WriteRequest {
            attrs: attrs.clone(),
            payload: b"950.00".to_vec(),
            ttl_seconds: 30,
            written_by: "split-test".into(),
            input_tuple_id: String::new(),
            txn_id: txn_id.clone(),
        })
        .await
        .expect("transactional write (auto-enlist path)");

    // Before commit: non-transactional read must NOT see the tuple.
    let hidden = space
        .read(ReadRequest {
            template: attrs.clone(),
            wait: false,
            timeout_ms: 0,
            txn_id: String::new(),
        })
        .await
        .expect("read pre-commit")
        .into_inner();
    assert!(
        hidden.tuple.is_none(),
        "uncommitted tuple must be invisible to non-txn reads"
    );

    // Commit: TxnMgr must drive Prepare + Commit into Space's participant.
    txn.commit(CommitRequest {
        txn_id: txn_id.clone(),
        wait_millis: 0,
    })
    .await
    .expect("commit round-trip (proves auto-enlist + 2PC wired)");

    // Post-commit: the tuple must now be visible.
    let visible = space
        .read(ReadRequest {
            template: attrs,
            wait: false,
            timeout_ms: 0,
            txn_id: String::new(),
        })
        .await
        .expect("read post-commit")
        .into_inner();
    let tuple = visible
        .tuple
        .expect("tuple should be visible after 2PC commit");
    assert_eq!(tuple.payload, b"950.00");
}

/// Abort path: transactional write, then abort via TxnMgr. TxnMgr's Abort
/// callback into Space must cause the uncommitted tuple to be discarded so
/// it never becomes visible.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn split_space_txn_auto_enlist_and_abort() {
    let (txn_addr, space_addr) = boot_stack().await;

    let mut space = SpaceServiceClient::connect(space_addr)
        .await
        .expect("dial Space");
    let mut txn = TransactionServiceClient::connect(txn_addr)
        .await
        .expect("dial TxnMgr");

    let created = txn
        .begin(BeginRequest { ttl_seconds: 30 })
        .await
        .expect("begin txn")
        .into_inner();
    let txn_id = created.txn_id;

    let mut attrs = HashMap::new();
    attrs.insert("ticker".to_string(), "TSLA".to_string());
    space
        .write(WriteRequest {
            attrs: attrs.clone(),
            payload: b"doomed".to_vec(),
            ttl_seconds: 30,
            written_by: "split-test".into(),
            input_tuple_id: String::new(),
            txn_id: txn_id.clone(),
        })
        .await
        .expect("transactional write");

    txn.abort(AbortRequest {
        txn_id,
        wait_millis: 0,
    })
    .await
    .expect("abort round-trip");

    let gone = space
        .read(ReadRequest {
            template: attrs,
            wait: false,
            timeout_ms: 0,
            txn_id: String::new(),
        })
        .await
        .expect("read post-abort")
        .into_inner();
    assert!(
        gone.tuple.is_none(),
        "aborted tuple must never become visible"
    );
}
