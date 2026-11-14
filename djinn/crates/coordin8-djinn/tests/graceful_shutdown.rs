//! Graceful shutdown: SIGTERM must exit 0 promptly, and a split-mode
//! service must deregister itself from Registry immediately (not after its
//! 30s self-lease TTL).
//!
//! Real `djinn` subprocesses, same pattern as
//! `self_registration_recovery.rs`. Unix-only — it signals with `kill(1)`.
#![cfg(unix)]

use std::collections::HashMap;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use coordin8_bootstrap::self_register;
use coordin8_proto::coordin8::registry_service_client::RegistryServiceClient;
use coordin8_proto::coordin8::{LookupRequest, RegistryWatchRequest};

mod common;
use common::{djinn, restart_registry_on, start_registry, KillOnDrop};

impl KillOnDrop {
    fn sigterm(&self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .expect("run kill(1)");
        assert!(status.success(), "kill -TERM failed");
    }

    /// Waits up to `timeout` for the child to exit on its own.
    async fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(st) = self.0.try_wait().expect("try_wait") {
                return Some(st);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn wait_until_accepting(host_port: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect(host_port).await.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "nothing accepting on {host_port} after {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn is_registered(registry_url: &str, interface: &str) -> bool {
    let Ok(channel) = tonic::transport::Channel::from_shared(registry_url.to_string())
        .unwrap()
        .connect()
        .await
    else {
        return false;
    };
    RegistryServiceClient::new(channel)
        .lookup(LookupRequest {
            template: HashMap::from([("interface".to_string(), interface.to_string())]),
        })
        .await
        .is_ok()
}

async fn wait_registered(registry_url: &str, interface: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !is_registered(registry_url, interface).await {
        assert!(
            Instant::now() < deadline,
            "{interface} never appeared in Registry"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Bundled `djinn` (fixed ports 9002-9006) exits 0 on SIGTERM, well inside
/// the default 20s grace period.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundled_djinn_exits_zero_on_sigterm() {
    let mut d = djinn(&[], &[]);
    wait_until_accepting("127.0.0.1:9002", Duration::from_secs(10)).await;
    // Let the bundled self-registrations land so deregistration is exercised.
    wait_registered("http://127.0.0.1:9002", "Space", Duration::from_secs(10)).await;

    let started = Instant::now();
    d.sigterm();
    let status = d
        .wait_exit(Duration::from_secs(8))
        .await
        .expect("djinn did not exit within 8s of SIGTERM");
    assert!(status.success(), "expected exit 0, got {status:?}");
    assert!(started.elapsed() < Duration::from_secs(8));
}

/// A split-mode service SIGTERMed while registered disappears from Registry
/// within ~2s (the self-lease TTL is 30s) and exits 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_service_deregisters_on_sigterm() {
    let (_registry, reg_hp) = start_registry(&[]).await;
    let reg_url = format!("http://{reg_hp}");

    let space_port = common::free_port().await;
    let mut space = djinn(
        &["space"],
        &[
            ("COORDIN8_BIND_ADDR", format!("127.0.0.1:{space_port}")),
            ("COORDIN8_REGISTRY", reg_url.clone()),
        ],
    );
    wait_registered(&reg_url, "Space", Duration::from_secs(10)).await;

    space.sigterm();
    let deadline = Instant::now() + Duration::from_secs(2);
    while is_registered(&reg_url, "Space").await {
        assert!(
            Instant::now() < deadline,
            "Space still in Registry 2s after SIGTERM — it lingers until TTL"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let status = space
        .wait_exit(Duration::from_secs(8))
        .await
        .expect("space did not exit after SIGTERM");
    assert!(status.success(), "expected exit 0, got {status:?}");
}

/// `SelfRegistrationHandle::cancel` must target the *post-recovery* lease.
/// Registry is restarted so the renewal task re-registers under a new lease
/// id the fresh Registry has never seen under the old one; cancel then has
/// to remove the entry immediately (a cancel of the initial, now-unknown
/// lease id would be a silent no-op and the entry would linger).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_uses_post_recovery_lease_id() {
    let (mut registry, hp) = start_registry(&[]).await;
    let url = format!("http://{hp}");

    let channel = tonic::transport::Channel::from_shared(url.clone())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let channel = coordin8_observability::wrap_traced_channel(
        channel,
        &coordin8_auth::ClientAuthConfig::trust(),
    );
    let handle = self_register(
        channel,
        "CancelRecoveryTest",
        HashMap::new(),
        "127.0.0.1",
        9999,
        3,
    )
    .await
    .expect("initial self-registration");
    let initial_lease = handle.initial_lease_id().to_string();

    registry.0.kill().unwrap();
    registry.0.wait().unwrap();
    let _fresh = restart_registry_on(&hp).await;

    // Wait for the renewal task to recover under a new lease.
    let deadline = Instant::now() + Duration::from_secs(10);
    while handle.current_lease_id() == initial_lease {
        assert!(Instant::now() < deadline, "never recovered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(is_registered(&url, "CancelRecoveryTest").await);

    handle.cancel().await.expect("cancel");
    assert!(
        !is_registered(&url, "CancelRecoveryTest").await,
        "cancel left the post-recovery entry in Registry"
    );
}

/// A client holding an open Registry `Watch` (every SDK `ServiceDiscovery`
/// does) must not stall the drain: on SIGTERM the stream ends with
/// UNAVAILABLE (so the client's reconnect loop kicks in rather than seeing a
/// clean EOF) and the process exits 0 far inside the grace period.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_watch_stream_ends_unavailable_and_exit_is_prompt() {
    let (mut registry, hp) =
        start_registry(&[("COORDIN8_SHUTDOWN_GRACE_SECS", "15".to_string())]).await;
    let url = format!("http://{hp}");

    let channel = tonic::transport::Channel::from_shared(url)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut stream = RegistryServiceClient::new(channel)
        .watch(RegistryWatchRequest {
            template: HashMap::new(),
        })
        .await
        .expect("open Watch")
        .into_inner();

    let started = Instant::now();
    registry.sigterm();

    let end = tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .expect("Watch stream still open 5s after SIGTERM");
    let status = end.expect_err("expected the stream to end with an error, not a clean EOF");
    assert_eq!(status.code(), tonic::Code::Unavailable, "{status:?}");

    let exit = registry
        .wait_exit(Duration::from_secs(5))
        .await
        .expect("registry did not exit promptly");
    assert!(exit.success(), "expected exit 0, got {exit:?}");
    assert!(started.elapsed() < Duration::from_secs(8));
}
