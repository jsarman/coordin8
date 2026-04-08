//! Integration tests for the `coordin8-bootstrap` public surface.
//!
//! Phase 0 covers `self_register` indirectly through the LeaseMgr boot path.
//! These tests exercise the remaining two entry points — `discover_lease_mgr`
//! and `watch_expiry_prefix` — before any downstream service leans on them.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;

use coordin8_bootstrap::{discover_lease_mgr, watch_expiry_prefix};
use coordin8_djinn::services::{run_lease_on_listener, run_registry_on_listener};
use coordin8_proto::coordin8::{ExpiryEvent, GrantRequest};

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

async fn spawn_lease(registry_addr: &str, ttl: u64) -> JoinHandle<()> {
    let (listener, _port) = ephemeral_listener().await;
    let registry_addr = registry_addr.to_string();
    let handle = tokio::spawn(async move {
        run_lease_on_listener(listener, Some(&registry_addr), "127.0.0.1", ttl)
            .await
            .ok();
    });
    // Give the server time to bind AND finish self-registration with Registry.
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle
}

// ── discover_lease_mgr ────────────────────────────────────────────────────────

/// Boot Registry + LeaseMgr, then have `discover_lease_mgr` look up the
/// LeaseMgr endpoint through Registry and return a working client.
/// Prove the client works by issuing a real Grant() call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discover_lease_mgr_finds_registered_lease_and_returns_working_client() {
    let (_reg, registry_addr) = spawn_registry().await;
    let _lease = spawn_lease(&registry_addr, 30).await;

    let mut client = discover_lease_mgr(&registry_addr)
        .await
        .expect("discover_lease_mgr");

    // Use the returned client to exercise the real LeaseMgr.
    let lease = client
        .grant(GrantRequest {
            resource_id: "bootstrap-test:sentinel".to_string(),
            ttl_seconds: 10,
        })
        .await
        .expect("grant via discovered client")
        .into_inner();

    assert!(
        !lease.lease_id.is_empty(),
        "discovered LeaseMgr should return a real lease_id"
    );
    assert_eq!(lease.resource_id, "bootstrap-test:sentinel");
}

// ── watch_expiry_prefix ───────────────────────────────────────────────────────

/// Spawn the `watch_expiry_prefix` loop against a live LeaseMgr, grant a
/// short-TTL lease whose resource_id matches the prefix, and confirm the
/// handler fires with the matching ExpiryEvent. Also grant a non-matching
/// lease to prove the prefix filter is actually filtering.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_expiry_prefix_receives_matching_events_only() {
    let (_reg, registry_addr) = spawn_registry().await;
    let _lease = spawn_lease(&registry_addr, 30).await;

    let client = discover_lease_mgr(&registry_addr)
        .await
        .expect("discover for watch");

    // Shared collector the handler writes to.
    let collected: Arc<Mutex<Vec<ExpiryEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let collected_for_handler = Arc::clone(&collected);

    let watcher = tokio::spawn(watch_expiry_prefix(
        client,
        registry_addr.clone(),
        "bootstrap-watch:".to_string(),
        move |evt| {
            collected_for_handler.lock().unwrap().push(evt);
        },
    ));

    // Give the stream a moment to open before leases are granted.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Grant two short leases directly through a fresh client:
    //   - matching the prefix → should surface in `collected`
    //   - not matching the prefix → should NOT surface
    let mut grant_client = discover_lease_mgr(&registry_addr)
        .await
        .expect("discover for grant");

    grant_client
        .grant(GrantRequest {
            resource_id: "bootstrap-watch:alpha".to_string(),
            ttl_seconds: 1,
        })
        .await
        .expect("grant alpha");

    grant_client
        .grant(GrantRequest {
            resource_id: "unrelated:beta".to_string(),
            ttl_seconds: 1,
        })
        .await
        .expect("grant beta");

    // TTL is 1s + reaper runs every 1s → wait 3s for both to expire.
    tokio::time::sleep(Duration::from_secs(3)).await;

    watcher.abort();
    let _ = watcher.await;

    let seen = collected.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        1,
        "expected exactly one matching event, got {seen:?}"
    );
    assert_eq!(seen[0].resource_id, "bootstrap-watch:alpha");
}
