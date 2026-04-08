//! Phase 0 integration test — Registry + LeaseMgr split boot.
//!
//! Proves the end-to-end Phase 0 contract:
//!   1. Registry starts alone on an ephemeral port.
//!   2. LeaseMgr starts, self-registers with that Registry (3s TTL).
//!   3. A client looking up `interface=LeaseMgr` finds exactly one entry with
//!      the correct advertise address.
//!   4. When the LeaseMgr task is aborted the entry self-cleans: after the TTL
//!      elapses a fresh lookup returns nothing.

use std::collections::HashMap;
use std::time::Duration;

use tokio::task::JoinHandle;

use coordin8_djinn::services::{run_lease_on_listener, run_registry_on_listener};
use coordin8_proto::coordin8::{
    lease_service_client::LeaseServiceClient, registry_service_client::RegistryServiceClient,
    LookupRequest,
};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Bind a TCP port 0, return the listener and the actual port number.
async fn ephemeral_listener() -> (tokio::net::TcpListener, u16) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}

/// Spawn Registry in a background task. Returns the task handle and the HTTP
/// address callers should use (e.g. "http://127.0.0.1:PORT").
async fn spawn_registry() -> (JoinHandle<()>, String) {
    let (listener, port) = ephemeral_listener().await;
    let addr = format!("http://127.0.0.1:{port}");
    let handle = tokio::spawn(async move {
        run_registry_on_listener(listener).await.ok();
    });
    // Give the server a moment to start accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (handle, addr)
}

/// Spawn LeaseMgr in a background task. Returns the task handle and the HTTP
/// URL that peers should use to reach it (host+port reconstructed).
async fn spawn_lease(registry_addr: &str, self_lease_ttl: u64) -> (JoinHandle<()>, String) {
    let (listener, port) = ephemeral_listener().await;
    let advertise_url = format!("http://127.0.0.1:{port}");
    let registry_addr = registry_addr.to_string();

    let handle = tokio::spawn(async move {
        run_lease_on_listener(listener, Some(&registry_addr), "127.0.0.1", self_lease_ttl)
            .await
            .ok();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    (handle, advertise_url)
}

// ── test ──────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase0_registry_and_lease_self_register_and_self_clean() {
    // --- 1. Boot Registry ---------------------------------------------------
    let (_reg_task, registry_addr) = spawn_registry().await;

    // --- 2. Boot LeaseMgr with a short 3s self-lease ------------------------
    let (lease_task, lease_advertise_addr) = spawn_lease(&registry_addr, 3).await;

    // Give LeaseMgr time to connect to Registry and complete self-registration.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // --- 3. Assert one LeaseMgr entry visible in Registry -------------------
    let mut reg_client = RegistryServiceClient::connect(registry_addr.clone())
        .await
        .expect("connect to registry");

    let template = {
        let mut m = HashMap::new();
        m.insert("interface".to_string(), "LeaseMgr".to_string());
        m
    };

    let cap = reg_client
        .lookup(LookupRequest {
            template: template.clone(),
        })
        .await
        .expect("lookup LeaseMgr")
        .into_inner();

    assert_eq!(
        cap.interface, "LeaseMgr",
        "expected interface=LeaseMgr, got {:?}",
        cap.interface
    );

    let transport_config = cap.transport.expect("transport must be present").config;
    let host = transport_config
        .get("host")
        .cloned()
        .expect("transport.host must be set");
    let port = transport_config
        .get("port")
        .cloned()
        .expect("transport.port must be set");
    let registered_addr = format!("http://{host}:{port}");

    assert_eq!(
        registered_addr, lease_advertise_addr,
        "registered address should match LeaseMgr advertise addr"
    );

    // Also verify the LeaseMgr is reachable at the registered address.
    LeaseServiceClient::connect(registered_addr.clone())
        .await
        .expect("should be able to connect to registered LeaseMgr address");

    // --- 4. Kill LeaseMgr, wait for TTL, assert entry gone ------------------
    lease_task.abort();
    let _ = lease_task.await; // ignore JoinError from abort

    // TTL is 3s + reaper runs every 1s. Wait 5s for reaper margin.
    tokio::time::sleep(Duration::from_secs(5)).await;

    let result = reg_client
        .lookup(LookupRequest {
            template: template.clone(),
        })
        .await;

    match result {
        Err(status) if status.code() == tonic::Code::NotFound => {
            // Correct — entry self-cleaned after lease expiry.
        }
        Err(e) => panic!("unexpected error from Registry lookup after TTL: {e}"),
        Ok(cap) => panic!(
            "expected NotFound after LeaseMgr death + TTL, got entry: {:?}",
            cap.into_inner()
        ),
    }
}
