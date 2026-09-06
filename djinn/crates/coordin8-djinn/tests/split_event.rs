//! Integration tests for split-mode EventMgr.
//!
//! Boots Registry + EventMgr as two separate services. EventMgr embeds its
//! own `LeaseManager` for subscription leases — no external LeaseMgr
//! dependency at all; Registry is only used for (optional) self-registration.
//!
//! Covers:
//!
//! 1. **End-to-end subscribe → emit → receive** through split-mode EventMgr.
//! 2. **Expiry propagation**: a short-TTL subscription must be cleaned up by
//!    EventMgr's own embedded reaper + cascade, so `receive` on the stale
//!    registration id returns NotFound.

use std::collections::HashMap;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use coordin8_djinn::services::{run_event_on_listener, run_registry_on_listener};
use coordin8_proto::coordin8::{
    event_service_client::EventServiceClient, EmitRequest, ReceiveRequest, SubscribeRequest,
};

// ── helpers ───────────────────────────────────────────────────────────────────

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

async fn spawn_event(registry_addr: &str) -> (JoinHandle<()>, String) {
    let (listener, port) = ephemeral_listener().await;
    let addr = format!("http://127.0.0.1:{port}");
    let registry_addr = registry_addr.to_string();
    let handle = tokio::spawn(async move {
        run_event_on_listener(listener, &registry_addr, "127.0.0.1", 30)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (handle, addr)
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Full split-mode path: Registry + EventMgr, each in its own task. A gRPC
/// client subscribes, emits, and receives through the split-mode EventMgr —
/// every lease grant is handled by EventMgr's own embedded LeaseManager.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_event_subscribe_emit_receive_round_trip() {
    let (_reg, registry_addr) = spawn_registry().await;
    let (_event, event_addr) = spawn_event(&registry_addr).await;

    let mut client = EventServiceClient::connect(event_addr)
        .await
        .expect("dial split-mode EventMgr");

    let sub = client
        .subscribe(SubscribeRequest {
            source: "split.test".into(),
            template: HashMap::new(),
            delivery: 0, // Durable
            ttl_seconds: 30,
            handback: b"hb".to_vec(),
        })
        .await
        .expect("subscribe (proves the embedded LeaseManager grant worked)")
        .into_inner();

    assert!(!sub.registration_id.is_empty());
    let lease = sub.lease.expect("subscription should carry a lease");
    assert!(
        !lease.grantor_host.is_empty() && lease.grantor_port != 0,
        "lease should carry a grantor address to renew against"
    );

    // Open the receive stream BEFORE emitting so live-stream delivery covers us
    // even if the mailbox drain path has nothing by the time we call.
    let mut stream = client
        .receive(ReceiveRequest {
            registration_id: sub.registration_id.clone(),
        })
        .await
        .expect("receive stream")
        .into_inner();

    // Give the receive task a moment to attach to the broadcast channel.
    tokio::time::sleep(Duration::from_millis(100)).await;

    client
        .emit(EmitRequest {
            source: "split.test".into(),
            event_type: "ping".into(),
            attrs: HashMap::new(),
            payload: b"hello split".to_vec(),
        })
        .await
        .expect("emit");

    let msg = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("receive did not yield in time")
        .expect("stream closed")
        .expect("stream error");

    assert_eq!(msg.source, "split.test");
    assert_eq!(msg.event_type, "ping");
    assert_eq!(msg.payload, b"hello split");
    assert_eq!(msg.handback, b"hb");
}

/// A subscription with a short TTL must be cleaned up on EventMgr when its
/// own embedded LeaseManager expires the lease. This exercises the cascade
/// wired in `run_event_on_listener` — without it, `unsubscribe_by_lease`
/// would never fire and the registration would leak.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_event_expiry_cleans_subscription() {
    let (_reg, registry_addr) = spawn_registry().await;
    let (_event, event_addr) = spawn_event(&registry_addr).await;

    let mut client = EventServiceClient::connect(event_addr)
        .await
        .expect("dial split-mode EventMgr");

    let sub = client
        .subscribe(SubscribeRequest {
            source: "split.expiry".into(),
            template: HashMap::new(),
            delivery: 0,
            ttl_seconds: 2, // short TTL — lease will expire
            handback: vec![],
        })
        .await
        .expect("subscribe")
        .into_inner();

    // Wait for: lease TTL (2s) + reaper tick (≤1s) + cascade to run.
    tokio::time::sleep(Duration::from_secs(4)).await;

    // A stale registration id should now be unknown to EventMgr.
    let err = client
        .receive(ReceiveRequest {
            registration_id: sub.registration_id.clone(),
        })
        .await
        .expect_err("receive on expired subscription should fail");

    assert_eq!(
        err.code(),
        tonic::Code::NotFound,
        "expected NotFound, got {err:?}"
    );
}
