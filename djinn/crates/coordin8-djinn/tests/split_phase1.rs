//! Phase 1 integration tests — Registry + LeaseMgr resilience.
//!
//! Builds on Phase 0 by deliberately killing/restarting services and running
//! redundant instances, proving the architecture survives instance loss:
//!
//! 1. **Restart.** Kill LeaseMgr, wait for TTL, bring it back. Registry must
//!    end up with a single *fresh* entry pointing at the new instance.
//! 2. **Redundancy.** Run two LeaseMgr instances concurrently. Both register.
//!    Kill one. The survivor's entry stays. `LookupAll` shows exactly one.

use std::collections::HashMap;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use coordin8_djinn::services::{run_lease_on_listener, run_registry_on_listener};
use coordin8_proto::coordin8::{registry_service_client::RegistryServiceClient, LookupRequest};

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

/// Spawn a LeaseMgr. Returns the task handle and the port it advertised.
async fn spawn_lease(registry_addr: &str, ttl: u64) -> (JoinHandle<()>, u16) {
    let (listener, port) = ephemeral_listener().await;
    let registry_addr = registry_addr.to_string();
    let handle = tokio::spawn(async move {
        run_lease_on_listener(listener, Some(&registry_addr), "127.0.0.1", ttl)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (handle, port)
}

fn lease_template() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("interface".to_string(), "LeaseMgr".to_string());
    m
}

async fn lookup_lease_port(
    client: &mut RegistryServiceClient<tonic::transport::Channel>,
) -> Option<u16> {
    let result = client
        .lookup(LookupRequest {
            template: lease_template(),
        })
        .await;
    match result {
        Ok(resp) => {
            let cap = resp.into_inner();
            let port = cap.transport?.config.get("port")?.parse().ok()?;
            Some(port)
        }
        Err(status) if status.code() == tonic::Code::NotFound => None,
        Err(e) => panic!("unexpected lookup error: {e}"),
    }
}

async fn lookup_all_lease_ports(
    client: &mut RegistryServiceClient<tonic::transport::Channel>,
) -> Vec<u16> {
    let mut stream = client
        .lookup_all(LookupRequest {
            template: lease_template(),
        })
        .await
        .expect("lookup_all")
        .into_inner();

    let mut ports = Vec::new();
    while let Some(item) = stream.next().await {
        let cap = item.expect("stream item");
        if let Some(t) = cap.transport {
            if let Some(p) = t.config.get("port").and_then(|s| s.parse().ok()) {
                ports.push(p);
            }
        }
    }
    ports.sort_unstable();
    ports
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Kill LeaseMgr, wait for TTL, bring a fresh instance back on a different
/// port. Registry must end up with one entry pointing at the new instance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase1_lease_restart_replaces_entry() {
    let (_reg_task, registry_addr) = spawn_registry().await;

    // Boot first LeaseMgr with a short 3s TTL.
    let (first_task, first_port) = spawn_lease(&registry_addr, 3).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut reg_client = RegistryServiceClient::connect(registry_addr.clone())
        .await
        .expect("connect to registry");

    assert_eq!(
        lookup_lease_port(&mut reg_client).await,
        Some(first_port),
        "first LeaseMgr should be registered"
    );

    // Kill first instance and wait for its self-lease to expire + reaper to fire.
    first_task.abort();
    let _ = first_task.await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    assert_eq!(
        lookup_lease_port(&mut reg_client).await,
        None,
        "entry should self-clean after TTL"
    );

    // Bring up a new LeaseMgr on a different ephemeral port.
    let (_second_task, second_port) = spawn_lease(&registry_addr, 3).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_ne!(
        first_port, second_port,
        "ephemeral ports should differ (sanity)"
    );
    assert_eq!(
        lookup_lease_port(&mut reg_client).await,
        Some(second_port),
        "restarted LeaseMgr should take over the entry"
    );
}

/// Run two LeaseMgr instances side-by-side. Both register. Kill one. The
/// survivor's entry stays — LookupAll returns exactly that survivor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phase1_redundant_lease_survives_one_kill() {
    let (_reg_task, registry_addr) = spawn_registry().await;

    let (task_a, port_a) = spawn_lease(&registry_addr, 3).await;
    let (_task_b, port_b) = spawn_lease(&registry_addr, 3).await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let mut reg_client = RegistryServiceClient::connect(registry_addr.clone())
        .await
        .expect("connect to registry");

    let mut expected = [port_a, port_b];
    expected.sort_unstable();
    assert_eq!(
        lookup_all_lease_ports(&mut reg_client).await,
        expected.to_vec(),
        "both LeaseMgrs should be visible"
    );

    // Kill instance A, wait for its self-lease to expire.
    task_a.abort();
    let _ = task_a.await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    assert_eq!(
        lookup_all_lease_ports(&mut reg_client).await,
        vec![port_b],
        "only the survivor's entry should remain"
    );
}
