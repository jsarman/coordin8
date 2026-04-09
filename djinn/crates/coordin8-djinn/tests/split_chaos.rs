//! Chaos tests for split-mode Djinn.
//!
//! These tests run a continuous workload against the split stack and kill
//! services mid-flight, asserting that:
//!
//! 1. **RemoteLeasing fails over transparently** when one of two LeaseMgrs
//!    dies. The workload (a driver looping grant/cancel) should see a short
//!    burst of transport errors during reconvergence, then recover. Total
//!    successes must dominate, and the final steady-state window must be
//!    error-free.
//!
//! 2. **A downstream service (Space) survives a LeaseMgr kill** end-to-end.
//!    Space holds its own `RemoteLeasing` and every `Write` grants a tuple
//!    lease over the wire. Killing one LeaseMgr must not break the service's
//!    workload beyond the reconvergence window.
//!
//! Reconvergence timing assumes LeaseMgr self-registers with a short TTL
//! (2–3 s) so that killing its tokio task stops renewal, the Registry entry
//! expires on its own TTL + reaper tick, and the next `Lookup` returns the
//! surviving instance. `discover_lease_mgr`'s exponential backoff then
//! retries until that surviving instance is reachable.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use coordin8_bootstrap::RemoteLeasing;
use coordin8_core::Leasing;
use coordin8_djinn::services::{
    run_lease_on_listener_with_shutdown, run_registry_on_listener, run_space_on_listener,
};
use coordin8_proto::coordin8::{space_service_client::SpaceServiceClient, WriteRequest};

// ── helpers ───────────────────────────────────────────────────────────────────

async fn ephemeral_listener() -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap()
}

async fn spawn_registry() -> (JoinHandle<()>, String) {
    let listener = ephemeral_listener().await;
    let port = listener.local_addr().unwrap().port();
    let addr = format!("http://127.0.0.1:{port}");
    let handle = tokio::spawn(async move {
        run_registry_on_listener(listener).await.ok();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (handle, addr)
}

/// A lease handle that can be killed via a real graceful-shutdown signal.
/// `abort()`-ing the JoinHandle is insufficient because tonic's
/// serve_with_incoming detaches per-connection tasks that continue serving
/// the existing HTTP/2 stream after the parent future is dropped. Sending on
/// `shutdown_tx` runs tonic's internal drain, which closes all connections.
struct LeaseHandle {
    handle: JoinHandle<()>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    port: u16,
}

impl LeaseHandle {
    /// Signal shutdown and wait briefly for the server task to drain. If
    /// draining stalls (e.g., tonic's graceful shutdown is waiting on an
    /// in-flight RPC whose upstream we also killed), the JoinHandle is
    /// aborted after the timeout. Combined with the listener being dropped
    /// as soon as shutdown fires, the port is reliably refused by the time
    /// this returns.
    async fn kill(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        match tokio::time::timeout(Duration::from_millis(500), &mut self.handle).await {
            Ok(_) => {}
            Err(_) => {
                self.handle.abort();
                let _ = self.handle.await;
            }
        }
    }
}

async fn spawn_lease(registry_addr: &str, ttl: u64) -> LeaseHandle {
    let listener = ephemeral_listener().await;
    let port = listener.local_addr().unwrap().port();
    let registry_addr = registry_addr.to_string();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        run_lease_on_listener_with_shutdown(
            listener,
            Some(&registry_addr),
            "127.0.0.1",
            ttl,
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
        .ok();
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    LeaseHandle {
        handle,
        shutdown_tx: Some(shutdown_tx),
        port,
    }
}

/// Diagnostic: verify a LeaseMgr port has stopped accepting TCP connections.
/// Abort-kill of the outer tokio task only reliably stops the server if the
/// inner future drops its listener before any per-connection task is
/// detached. If this returns `true` after `abort()`, the kill did not
/// actually close the port and the chaos test is not simulating a crash.
async fn port_is_refused(port: u16) -> bool {
    tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .is_err()
}

async fn spawn_space(registry_addr: &str) -> (JoinHandle<()>, String) {
    let listener = ephemeral_listener().await;
    let port = listener.local_addr().unwrap().port();
    let addr = format!("http://127.0.0.1:{port}");
    let registry_addr = registry_addr.to_string();
    let handle = tokio::spawn(async move {
        run_space_on_listener(listener, &registry_addr, "127.0.0.1", 30)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    (handle, addr)
}

/// Three-phase counters. The workload runs across three windows — pre-kill,
/// after one LeaseMgr dies, and after both die — and we assert on the shape
/// of success counts across those windows. That shape is the proof:
///
/// - Phase 0 successes > 0 → workload is healthy.
/// - Phase 1 successes > 0 → the survivor took over (failover worked).
/// - Phase 2 successes = 0 → killing the survivor really stops the workload,
///   which means Phase 1 was actually running on the survivor (not still on
///   the supposedly-dead first instance).
#[derive(Default)]
struct Counters {
    phase: AtomicU64, // 0 = pre-kill, 1 = after A killed, 2 = after B killed
    phase0_successes: AtomicU64,
    phase1_successes: AtomicU64,
    phase2_successes: AtomicU64,
    phase2_errors: AtomicU64,
    total_errors: AtomicU64,
}

impl Counters {
    fn record_success(&self) {
        match self.phase.load(Ordering::Relaxed) {
            0 => self.phase0_successes.fetch_add(1, Ordering::Relaxed),
            1 => self.phase1_successes.fetch_add(1, Ordering::Relaxed),
            _ => self.phase2_successes.fetch_add(1, Ordering::Relaxed),
        };
    }
    fn record_error(&self) {
        self.total_errors.fetch_add(1, Ordering::Relaxed);
        if self.phase.load(Ordering::Relaxed) >= 2 {
            self.phase2_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// ── Test A ────────────────────────────────────────────────────────────────────

/// Three-phase chaos against `RemoteLeasing`:
///
/// - **Phase 0** (baseline): lease_a + lease_b running; RemoteLeasing pinned
///   to lease_a. Workload accumulates successes.
/// - **Phase 1** (after lease_a killed): lease_b running; RemoteLeasing
///   should rediscover and land on lease_b. Workload continues to accumulate
///   successes — this is the failover proof.
/// - **Phase 2** (after lease_b killed): no LeaseMgr running. Workload MUST
///   stop succeeding. If Phase 2 still sees successes, it means Phase 1 was
///   actually running on the "dead" lease_a (e.g., a detached tonic task),
///   and the test's failover claim is a false positive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos_remote_leasing_survives_leasemgr_kill() {
    let (_reg, registry_addr) = spawn_registry().await;

    // 3s TTL so killed instances drop out of Registry fast.
    let lease_a = spawn_lease(&registry_addr, 3).await;
    let lease_a_port = lease_a.port;

    let leasing = Arc::new(
        RemoteLeasing::connect(&registry_addr)
            .await
            .expect("RemoteLeasing::connect"),
    );

    // Pin RemoteLeasing's internal Channel to lease_a before lease_b exists.
    let pin = leasing
        .grant("chaos:pin-a", 5)
        .await
        .expect("pin grant against lease_a");
    leasing.cancel(&pin.lease_id).await.ok();

    // Now bring up lease_b. RemoteLeasing has no knowledge of it yet.
    let lease_b = spawn_lease(&registry_addr, 3).await;
    let lease_b_port = lease_b.port;

    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));

    let driver_leasing = Arc::clone(&leasing);
    let driver_counters = Arc::clone(&counters);
    let driver_stop = Arc::clone(&stop);
    // Each grant is wrapped in a short timeout. When both LeaseMgrs are
    // dead, RemoteLeasing's internal `retry_forever` would otherwise hang
    // the driver indefinitely — the timeout is what lets the driver count
    // errors and keep observing the stop flag in phase 2.
    let driver = tokio::spawn(async move {
        let mut i: u64 = 0;
        while !driver_stop.load(Ordering::Relaxed) {
            i += 1;
            let resource_id = format!("chaos:workload:{i}");
            let op = driver_leasing.grant(&resource_id, 5);
            match tokio::time::timeout(Duration::from_millis(500), op).await {
                Ok(Ok(record)) => {
                    driver_counters.record_success();
                    let _ = tokio::time::timeout(
                        Duration::from_millis(500),
                        driver_leasing.cancel(&record.lease_id),
                    )
                    .await;
                }
                _ => driver_counters.record_error(),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    // Phase 0: baseline.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Phase 1: kill lease_a, workload should fail over to lease_b.
    counters.phase.store(1, Ordering::Relaxed);
    lease_a.kill().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        port_is_refused(lease_a_port).await,
        "lease_a port {lease_a_port} still accepting connections after shutdown"
    );
    // Give the workload time to rediscover and accumulate phase-1 successes.
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Phase 2: kill lease_b. Nothing should succeed after this.
    counters.phase.store(2, Ordering::Relaxed);
    lease_b.kill().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        port_is_refused(lease_b_port).await,
        "lease_b port {lease_b_port} still accepting connections after shutdown"
    );
    // Observation window for phase 2. Each driver iteration can take up to
    // ~550ms (500ms grant timeout + 50ms sleep) while both LeaseMgrs are
    // dead, so we need enough wall-clock for several iterations to count.
    tokio::time::sleep(Duration::from_secs(5)).await;

    stop.store(true, Ordering::Relaxed);
    driver.await.ok();

    let p0 = counters.phase0_successes.load(Ordering::Relaxed);
    let p1 = counters.phase1_successes.load(Ordering::Relaxed);
    let p2 = counters.phase2_successes.load(Ordering::Relaxed);
    let p2_errs = counters.phase2_errors.load(Ordering::Relaxed);
    let total_errs = counters.total_errors.load(Ordering::Relaxed);

    println!(
        "chaos A results: phase0={p0} phase1={p1} phase2={p2} \
         phase2_errors={p2_errs} total_errors={total_errs}"
    );

    assert!(p0 > 5, "phase 0 baseline too low: {p0} (expected > 5)");

    // FAILOVER PROOF: lease_b took over after lease_a died.
    assert!(
        p1 > 10,
        "phase 1 successes too low: {p1} — killing lease_a broke the workload \
         (failover did not happen)"
    );

    // CRASH PROOF: killing lease_b (the survivor) really stops the workload.
    assert!(
        p2_errs > p2,
        "phase 2 errors ({p2_errs}) must dominate phase 2 successes ({p2}) — \
         otherwise workload was still talking to a server that should be dead"
    );
    assert!(
        p2_errs >= 3,
        "phase 2 errors too low: {p2_errs} — killing lease_b did not affect \
         the workload, so phase 1 was not running on lease_b"
    );
}

// ── Test B ────────────────────────────────────────────────────────────────────

/// End-to-end chaos: drive continuous `Write` calls against a split Space
/// while killing one of two LeaseMgrs. Space holds its own `RemoteLeasing`,
/// so each `Write` involves an over-the-wire grant. The test proves that a
/// downstream service survives LeaseMgr failover without the test harness
/// holding RemoteLeasing directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos_split_space_survives_leasemgr_kill() {
    let (_reg, registry_addr) = spawn_registry().await;

    // LeaseMgr A alone — Space's internal RemoteLeasing will discover A
    // during boot and stay pinned to it. B comes up after Space is already
    // running so Space only learns about it via rediscovery after A dies.
    let lease_a = spawn_lease(&registry_addr, 3).await;
    let lease_a_port = lease_a.port;

    let (_space, space_addr) = spawn_space(&registry_addr).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let lease_b = spawn_lease(&registry_addr, 3).await;
    let lease_b_port = lease_b.port;

    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));

    // Driver: tight Write loop against Space. Each write requires Space to
    // issue a `space:` grant through its `RemoteLeasing` — that's the code
    // path under test.
    let driver_counters = Arc::clone(&counters);
    let driver_stop = Arc::clone(&stop);
    let driver_addr = space_addr.clone();
    let driver = tokio::spawn(async move {
        let mut client = SpaceServiceClient::connect(driver_addr.clone())
            .await
            .expect("dial split Space");

        let mut i: u64 = 0;
        while !driver_stop.load(Ordering::Relaxed) {
            i += 1;
            let mut attrs = HashMap::new();
            attrs.insert("chaos".into(), "b".into());
            attrs.insert("seq".into(), i.to_string());

            let req = WriteRequest {
                attrs,
                payload: format!("chaos-write-{i}").into_bytes(),
                ttl_seconds: 5,
                written_by: "chaos-driver".into(),
                input_tuple_id: String::new(),
                txn_id: String::new(),
            };

            // Same timeout story as test A: once both LeaseMgrs die, Space's
            // internal `grant` call is stuck in retry_forever, and without a
            // timeout the Write RPC never returns.
            match tokio::time::timeout(Duration::from_millis(500), client.write(req)).await {
                Ok(Ok(_)) => driver_counters.record_success(),
                _ => driver_counters.record_error(),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    // Phase 0: baseline through Space.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Phase 1: kill lease_a, Space's internal RemoteLeasing should fail over.
    counters.phase.store(1, Ordering::Relaxed);
    lease_a.kill().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        port_is_refused(lease_a_port).await,
        "lease_a port {lease_a_port} still accepting connections after shutdown"
    );
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Phase 2: kill lease_b. Writes must start failing — a Write without a
    // grant is a hard error inside Space's service code.
    counters.phase.store(2, Ordering::Relaxed);
    lease_b.kill().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        port_is_refused(lease_b_port).await,
        "lease_b port {lease_b_port} still accepting connections after shutdown"
    );
    tokio::time::sleep(Duration::from_secs(5)).await;

    stop.store(true, Ordering::Relaxed);
    driver.await.ok();

    let p0 = counters.phase0_successes.load(Ordering::Relaxed);
    let p1 = counters.phase1_successes.load(Ordering::Relaxed);
    let p2 = counters.phase2_successes.load(Ordering::Relaxed);
    let p2_errs = counters.phase2_errors.load(Ordering::Relaxed);
    let total_errs = counters.total_errors.load(Ordering::Relaxed);

    println!(
        "chaos B results: phase0={p0} phase1={p1} phase2={p2} \
         phase2_errors={p2_errs} total_errors={total_errs}"
    );

    assert!(p0 > 5, "phase 0 baseline too low: {p0}");
    assert!(
        p1 > 10,
        "phase 1 Space writes too low: {p1} — Space did not fail over"
    );
    assert!(
        p2_errs > p2,
        "phase 2 errors ({p2_errs}) must dominate phase 2 successes ({p2}) — \
         otherwise Space was still talking to a LeaseMgr that should be dead"
    );
    assert!(
        p2_errs >= 3,
        "phase 2 errors too low: {p2_errs} — killing lease_b had no effect"
    );
}
