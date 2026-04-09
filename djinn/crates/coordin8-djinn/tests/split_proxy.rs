//! Integration tests for split-mode Proxy.
//!
//! Boots Registry + LeaseMgr + Proxy as three separate services, stands up a
//! dummy TCP echo server, self-registers the echo server into Registry, then
//! opens a proxy through the split Proxy via gRPC and confirms bytes round
//! trip from a TCP client through the proxy to the echo server.
//!
//! Split-mode Proxy resolves templates through `RemoteCapabilityResolver`,
//! so every open and every forwarded TCP connection crosses a Registry
//! `Lookup` RPC — nothing is shared in-process between Proxy and Registry.

use std::collections::HashMap;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

use coordin8_bootstrap::self_register;
use coordin8_djinn::services::{
    run_lease_on_listener, run_proxy_on_listener, run_registry_on_listener,
};
use coordin8_proto::coordin8::{
    proxy_service_client::ProxyServiceClient, registry_service_client::RegistryServiceClient,
    OpenRequest,
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

async fn spawn_proxy(registry_addr: &str) -> (JoinHandle<()>, String) {
    let (listener, port) = ephemeral_listener().await;
    let addr = format!("http://127.0.0.1:{port}");
    let registry_addr = registry_addr.to_string();
    let handle = tokio::spawn(async move {
        run_proxy_on_listener(listener, &registry_addr, "127.0.0.1", 30)
            .await
            .ok();
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    (handle, addr)
}

/// A tiny TCP echo server. Accepts one connection, echoes whatever it
/// receives until the client closes, then exits. We intentionally keep the
/// task simple — the test only opens a single connection.
async fn spawn_echo_server() -> (JoinHandle<()>, u16) {
    let (listener, port) = ephemeral_listener().await;
    let handle = tokio::spawn(async move {
        if let Ok((mut sock, _peer)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if sock.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });
    (handle, port)
}

/// Full split-mode proxy round trip:
///   client ──TCP──▶ split-mode Proxy ──TCP──▶ upstream echo server
///                    │
///                    └──gRPC Lookup─▶ split-mode Registry
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn split_proxy_forwards_through_remote_registry() {
    let (_reg, registry_addr) = spawn_registry().await;
    let _lease = spawn_lease(&registry_addr, 30).await;
    let (_proxy, proxy_addr) = spawn_proxy(&registry_addr).await;

    // Stand up a TCP echo server and self-register it under a unique
    // interface so the Proxy's RemoteCapabilityResolver finds exactly it.
    let (_echo, echo_port) = spawn_echo_server().await;

    let registry_client = RegistryServiceClient::connect(registry_addr.clone())
        .await
        .expect("dial Registry from test");
    let _handle = self_register(
        registry_client,
        "SplitProxyEcho",
        HashMap::new(),
        "127.0.0.1",
        echo_port,
        30,
    )
    .await
    .expect("register echo server in Registry");

    // Give the Registry a moment to commit the entry.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Open a proxy through the split-mode ProxyService.
    let mut proxy_client = ProxyServiceClient::connect(proxy_addr)
        .await
        .expect("dial split-mode Proxy");

    let mut template = HashMap::new();
    template.insert("interface".to_string(), "SplitProxyEcho".to_string());

    let handle = proxy_client
        .open(OpenRequest { template })
        .await
        .expect("open (proves RemoteCapabilityResolver.resolve worked)")
        .into_inner();

    assert!(!handle.proxy_id.is_empty());
    assert!(handle.local_port > 0);

    // Round-trip some bytes through the local proxy port → echo server.
    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", handle.local_port))
        .await
        .expect("connect to proxy local port");

    stream
        .write_all(b"hello proxy")
        .await
        .expect("write to proxy");

    let mut buf = [0u8; 11];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut buf))
        .await
        .expect("echo did not respond in time")
        .expect("read from proxy");

    assert_eq!(&buf, b"hello proxy");
}
