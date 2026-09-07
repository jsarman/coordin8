//! Regression test for a code-review finding (2026-09-07): self-registration
//! previously could not recover once its Registry entry expired while
//! unreachable — every subsequent renewal cited the now-unknown
//! `capability_id` and got `NotFound` forever, leaving the service running
//! but permanently invisible in Registry.
//!
//! Reproduces the failure mode as faithfully as an in-process test can:
//! Registry runs as a real *subprocess* (not just another tokio task in
//! this test binary), so killing it actually closes its sockets at the OS
//! level the way a real crash/restart would — an in-process `JoinHandle::
//! abort()` doesn't, since tonic spawns one detached task per accepted
//! connection and aborting the accept-loop task doesn't cancel those, so an
//! already-connected client would keep talking to a "dead" registry's
//! zombie connection handler and never actually observe anything wrong.
//!
//! Exercises the real `coordin8_bootstrap::self_register` renewal task
//! (not a hand-rolled replica of it), so this actually proves the fix
//! rather than just the server's NotFound contract.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use coordin8_bootstrap::self_register;
use coordin8_proto::coordin8::registry_service_client::RegistryServiceClient;
use coordin8_proto::coordin8::LookupRequest;

/// Kills the wrapped child on drop, so a panicking assertion (which skips
/// the rest of the test function) still doesn't leak the subprocess.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_registry_process(port: u16) -> KillOnDrop {
    let child = Command::new(env!("CARGO_BIN_EXE_djinn"))
        .arg("registry")
        .env("COORDIN8_BIND_ADDR", format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn `djinn registry` subprocess");
    KillOnDrop(child)
}

async fn dial(addr: &str) -> RegistryServiceClient<tonic::transport::Channel> {
    let channel = tonic::transport::Channel::from_shared(addr.to_string())
        .unwrap()
        .connect()
        .await
        .expect("dial Registry");
    RegistryServiceClient::new(channel)
}

/// `self_register` requires the same `TracedAuthedChannel`-wrapped client
/// every real call site uses (Decision 8 / Decision 2 of the auth and
/// observability PRDs) — auth itself is a no-op here since no
/// `COORDIN8_JWT_SECRET` is set for the subprocess.
async fn dial_authed(
    addr: &str,
) -> RegistryServiceClient<coordin8_observability::TracedAuthedChannel> {
    let channel = tonic::transport::Channel::from_shared(addr.to_string())
        .unwrap()
        .connect()
        .await
        .expect("dial Registry");
    RegistryServiceClient::new(coordin8_observability::wrap_traced_channel(
        channel,
        &coordin8_auth::ClientAuthConfig::trust(),
    ))
}

/// Polls until a plain TCP connect to `addr` succeeds, or panics after
/// `timeout`. Used to wait for the (real, separately-booting) subprocess to
/// actually be accepting connections before the test proceeds.
async fn wait_until_accepting(host_port: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect(host_port).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("nothing accepting connections on {host_port} after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn interface_is_registered(addr: &str, interface: &str) -> bool {
    let mut client = dial(addr).await;
    client
        .lookup(LookupRequest {
            template: std::collections::HashMap::from([(
                "interface".to_string(),
                interface.to_string(),
            )]),
        })
        .await
        .is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn self_registration_recovers_after_registry_restarts() {
    // Grab a probably-free ephemeral port, then release it immediately —
    // the subprocess needs to bind it itself, so this can't hold the
    // listener open. A tiny TOCTOU window is an acceptable trade-off here.
    let port = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let host_port = format!("127.0.0.1:{port}");
    let addr = format!("http://{host_port}");

    let mut registry = spawn_registry_process(port);
    wait_until_accepting(&host_port, Duration::from_secs(5)).await;

    // Short TTL (renewal interval = ttl.max(3)/3 = 1s) so the test doesn't
    // need to wait long for the fix's re-registration path to kick in.
    let client = dial_authed(&addr).await;
    let _handle = self_register(
        client,
        "SelfRegRecoveryTest",
        Default::default(),
        "127.0.0.1",
        9999,
        3,
    )
    .await
    .expect("initial self-registration");

    assert!(
        interface_is_registered(&addr, "SelfRegRecoveryTest").await,
        "expected the initial registration to be visible"
    );

    // Simulate a real Registry crash/restart: kill the process (closes its
    // sockets at the OS level — unlike aborting an in-process task, which
    // wouldn't affect tonic's already-spawned, detached per-connection
    // tasks) and bring up a fresh one on the same port. Its in-memory store
    // has never heard of this registration's capability_id, so the
    // renewal task's next tick is guaranteed to hit NotFound.
    registry.0.kill().expect("kill registry subprocess");
    registry.0.wait().expect("reap registry subprocess");
    let _fresh_registry = spawn_registry_process(port);
    wait_until_accepting(&host_port, Duration::from_secs(5)).await;

    // Poll rather than a single fixed sleep: the renewal interval is ~1s,
    // and re-registration takes one more RPC round trip after the NotFound.
    let mut recovered = false;
    for _ in 0..50 {
        if interface_is_registered(&addr, "SelfRegRecoveryTest").await {
            recovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    assert!(
        recovered,
        "self-registration never recovered after the Registry process restarted — \
         the renewal task is still citing a stale capability_id instead of \
         re-registering on NotFound"
    );
}
