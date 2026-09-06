//! Integration tests for split-mode Space.
//!
//! Boots Registry + Space as two separate services. Space embeds its own
//! `LeaseManager` for tuple and watch leases — no external LeaseMgr
//! dependency at all; Registry is only used for (optional) self-registration.

use std::collections::HashMap;
use std::time::Duration;

use tokio::task::JoinHandle;

use coordin8_djinn::services::{run_registry_on_listener, run_space_on_listener};
use coordin8_proto::coordin8::{
    space_service_client::SpaceServiceClient, ReadRequest, TakeRequest, WriteRequest,
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

async fn spawn_space(registry_addr: &str) -> (JoinHandle<()>, String) {
    let (listener, port) = ephemeral_listener().await;
    let addr = format!("http://127.0.0.1:{port}");
    let registry_addr = registry_addr.to_string();
    let handle = tokio::spawn(async move {
        run_space_on_listener(listener, &registry_addr, "127.0.0.1", 30)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (handle, addr)
}

/// Full split-mode round trip: write a tuple, read it back by template,
/// then take it. Every lease grant is handled by Space's own embedded
/// LeaseManager.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_space_write_read_take_round_trip() {
    let (_reg, registry_addr) = spawn_registry().await;
    let (_space, space_addr) = spawn_space(&registry_addr).await;

    let mut client = SpaceServiceClient::connect(space_addr)
        .await
        .expect("dial split-mode Space");

    let mut attrs = HashMap::new();
    attrs.insert("ticker".to_string(), "AAPL".to_string());

    let write_resp = client
        .write(WriteRequest {
            attrs: attrs.clone(),
            payload: b"150.25".to_vec(),
            ttl_seconds: 30,
            written_by: "split-test".into(),
            input_tuple_id: String::new(),
            txn_id: String::new(),
        })
        .await
        .expect("write (proves the embedded LeaseManager grant worked)")
        .into_inner();

    let written_tuple = write_resp.tuple.expect("write returned tuple");
    let lease = written_tuple
        .lease
        .clone()
        .expect("tuple should carry a lease");
    assert!(
        !lease.grantor_host.is_empty() && lease.grantor_port != 0,
        "lease should carry a grantor address to renew against"
    );
    assert!(!written_tuple.tuple_id.is_empty());

    let read_resp = client
        .read(ReadRequest {
            template: attrs.clone(),
            wait: false,
            timeout_ms: 0,
            txn_id: String::new(),
        })
        .await
        .expect("read")
        .into_inner();

    let read_tuple = read_resp.tuple.expect("read found tuple");
    assert_eq!(read_tuple.tuple_id, written_tuple.tuple_id);
    assert_eq!(read_tuple.payload, b"150.25");

    let take_resp = client
        .take(TakeRequest {
            template: attrs.clone(),
            wait: false,
            timeout_ms: 0,
            txn_id: String::new(),
        })
        .await
        .expect("take")
        .into_inner();

    let taken = take_resp.tuple.expect("take found tuple");
    assert_eq!(taken.tuple_id, written_tuple.tuple_id);

    // Second take should return empty — the tuple was consumed.
    let second_take = client
        .take(TakeRequest {
            template: attrs,
            wait: false,
            timeout_ms: 0,
            txn_id: String::new(),
        })
        .await
        .expect("second take")
        .into_inner();
    assert!(
        second_take.tuple.is_none(),
        "tuple should have been consumed by first take"
    );
}

/// A short-TTL tuple written through split-mode Space must be cleaned up
/// when its own embedded LeaseManager expires the `space:` lease. This
/// exercises the cascade wired into `run_space_on_listener` — without it,
/// `on_tuple_expired` never fires and the tuple stays visible.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_space_expiry_reaps_tuple() {
    let (_reg, registry_addr) = spawn_registry().await;
    let (_space, space_addr) = spawn_space(&registry_addr).await;

    let mut client = SpaceServiceClient::connect(space_addr)
        .await
        .expect("dial split-mode Space");

    let mut attrs = HashMap::new();
    attrs.insert("kind".to_string(), "ephemeral".to_string());

    client
        .write(WriteRequest {
            attrs: attrs.clone(),
            payload: b"will-expire".to_vec(),
            ttl_seconds: 2,
            written_by: "split-test".into(),
            input_tuple_id: String::new(),
            txn_id: String::new(),
        })
        .await
        .expect("write short-TTL tuple");

    // Sanity: tuple is there right now.
    let present = client
        .read(ReadRequest {
            template: attrs.clone(),
            wait: false,
            timeout_ms: 0,
            txn_id: String::new(),
        })
        .await
        .expect("read before expiry")
        .into_inner();
    assert!(present.tuple.is_some(), "tuple should exist before TTL");

    // Wait for: TTL (2s) + reaper tick (≤1s) + cascade to run.
    tokio::time::sleep(Duration::from_secs(4)).await;

    let after = client
        .read(ReadRequest {
            template: attrs,
            wait: false,
            timeout_ms: 0,
            txn_id: String::new(),
        })
        .await
        .expect("read after expiry")
        .into_inner();
    assert!(
        after.tuple.is_none(),
        "tuple should have been reaped by the embedded LeaseManager cascade"
    );
}
