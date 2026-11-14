//! Shared helpers for tests that run real `djinn` subprocesses.
//!
//! A subprocess has to bind its own port, so a test picks a free one and
//! releases it first. Between the release and the child's bind another
//! process (parallel tests, a bundled Djinn's outbound sockets) can take
//! it; the child then fails to bind and exits. A bare TCP connect does not
//! notice: on loopback a connect to an unbound ephemeral port can succeed
//! by connecting to itself. So readiness here means "the child is alive and
//! answered a gRPC call", and a child that died at boot is retried on a
//! fresh port.
#![allow(dead_code)] // each test binary uses a subset

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use coordin8_proto::coordin8::registry_service_client::RegistryServiceClient;
use coordin8_proto::coordin8::LookupRequest;

/// Kills the wrapped child on drop so a failing assertion doesn't leak it.
pub struct KillOnDrop(pub Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn djinn(args: &[&str], envs: &[(&str, String)]) -> KillOnDrop {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_djinn"));
    cmd.args(args).stdout(Stdio::null()).stderr(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    KillOnDrop(cmd.spawn().expect("spawn djinn subprocess"))
}

pub async fn free_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

/// True once a Registry at `url` answers a Lookup (any gRPC status counts:
/// a NotFound came from a real server).
async fn registry_answers(url: &str) -> bool {
    let Ok(channel) = tonic::transport::Channel::from_shared(url.to_string())
        .unwrap()
        .connect()
        .await
    else {
        return false;
    };
    let res = RegistryServiceClient::new(channel)
        .lookup(LookupRequest {
            template: HashMap::from([("interface".to_string(), "__probe__".to_string())]),
        })
        .await;
    match res {
        Ok(_) => true,
        Err(s) => !matches!(s.code(), tonic::Code::Unavailable | tonic::Code::Unknown),
    }
}

/// Waits until `child` serves the Registry at `url`. Returns false if the
/// child exits first (its port was taken); panics after `timeout`.
pub async fn wait_registry_ready(child: &mut KillOnDrop, url: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if child.0.try_wait().expect("try_wait").is_some() {
            return false;
        }
        if registry_answers(url).await {
            return true;
        }
        assert!(
            Instant::now() < deadline,
            "Registry at {url} not serving after {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Starts `djinn registry` on a free port and waits until it serves.
/// Returns the child and its `host:port`.
pub async fn start_registry(envs: &[(&str, String)]) -> (KillOnDrop, String) {
    for _ in 0..5 {
        let hp = format!("127.0.0.1:{}", free_port().await);
        let mut all = vec![("COORDIN8_BIND_ADDR", hp.clone())];
        all.extend(envs.iter().cloned());
        let mut child = djinn(&["registry"], &all);
        if wait_registry_ready(&mut child, &format!("http://{hp}"), Duration::from_secs(10)).await {
            return (child, hp);
        }
    }
    panic!("djinn registry failed to start on 5 different ports");
}

/// Restarts a Registry on the same `hp` (tests that need the address to
/// survive a restart). Retries briefly if the port is momentarily busy.
pub async fn restart_registry_on(hp: &str) -> KillOnDrop {
    for _ in 0..20 {
        let mut child = djinn(&["registry"], &[("COORDIN8_BIND_ADDR", hp.to_string())]);
        if wait_registry_ready(&mut child, &format!("http://{hp}"), Duration::from_secs(10)).await {
            return child;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("djinn registry could not rebind {hp}");
}
