//! Bootstrap helpers for split-mode Djinn services.
//!
//! This crate sits between `coordin8-core` (no proto dependency) and the
//! service crates. It owns the three primitives that every service needs when
//! running outside the monolith:
//!
//! - **[`discover_lease_mgr`]** — find LeaseMgr in Registry, retry forever.
//! - **[`self_register`]** — insert a Registry entry and keep it alive via
//!   periodic re-registration, cancel on drop.
//! - **[`watch_expiry_prefix`]** — stream `WatchExpiry` events for a given
//!   resource-id prefix, reconnect automatically.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tonic::transport::Channel;
use tracing::{debug, info, warn};

use coordin8_proto::coordin8::{
    lease_service_client::LeaseServiceClient, registry_service_client::RegistryServiceClient,
    ExpiryEvent, LookupRequest, RegisterRequest, TransportDescriptor, WatchExpiryRequest,
};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors returned by bootstrap operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// gRPC transport error.
    #[error("transport: {0}")]
    Transport(#[from] tonic::transport::Error),

    /// gRPC status error.
    #[error("rpc: {0}")]
    Status(#[from] tonic::Status),

    /// A capability returned by Registry is missing required transport fields.
    #[error("capability missing transport field: {0}")]
    MissingTransport(&'static str),
}

// ── Shared backoff helper ─────────────────────────────────────────────────────

/// Retry an async operation with exponential backoff, capped at 5s, forever.
///
/// Starts at 100ms and doubles on each failure. Logs a warning on every retry
/// using `label` for structured context.
async fn retry_forever<T, E, F, Fut>(label: &'static str, mut op: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut backoff_ms: u64 = 100;
    loop {
        match op().await {
            Ok(v) => return v,
            Err(e) => {
                warn!(op = label, backoff_ms, "retrying: {e}");
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(5_000);
            }
        }
    }
}

// ── discover_lease_mgr ────────────────────────────────────────────────────────

/// Connect to Registry and discover a live LeaseMgr instance.
///
/// Polls the Registry at `registry_addr` for an entry with
/// `interface = "LeaseMgr"`. Retries with exponential backoff starting at
/// 100 ms, capped at 5 s, forever. Returns a ready-to-use
/// [`LeaseServiceClient`] connected to the discovered address.
///
/// # Example
///
/// ```no_run
/// # #[tokio::main] async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let lease = coordin8_bootstrap::discover_lease_mgr("http://127.0.0.1:9002").await?;
/// # Ok(()) }
/// ```
pub async fn discover_lease_mgr(registry_addr: &str) -> Result<LeaseServiceClient<Channel>, Error> {
    let client = retry_forever("discover_lease_mgr", || async {
        try_discover_lease_mgr(registry_addr).await
    })
    .await;
    info!(registry = registry_addr, "discovered LeaseMgr");
    Ok(client)
}

/// Single attempt to discover LeaseMgr via a Registry lookup.
async fn try_discover_lease_mgr(registry_addr: &str) -> Result<LeaseServiceClient<Channel>, Error> {
    let channel = Channel::from_shared(registry_addr.to_string())
        .map_err(|_| Error::MissingTransport("invalid registry address"))?
        .connect()
        .await?;

    let mut registry = RegistryServiceClient::new(channel);

    let template = HashMap::from([("interface".to_string(), "LeaseMgr".to_string())]);

    let cap = registry
        .lookup(LookupRequest { template })
        .await?
        .into_inner();

    let transport = cap
        .transport
        .as_ref()
        .ok_or(Error::MissingTransport("transport"))?;
    let host = transport
        .config
        .get("host")
        .ok_or(Error::MissingTransport("host"))?;
    let port = transport
        .config
        .get("port")
        .ok_or(Error::MissingTransport("port"))?;
    let addr = format!("http://{host}:{port}");

    debug!(addr, "connecting to LeaseMgr");
    let lease_channel = Channel::from_shared(addr)
        .map_err(|_| Error::MissingTransport("host/port formed invalid URL"))?
        .connect()
        .await?;

    Ok(LeaseServiceClient::new(lease_channel))
}

// ── SelfRegistrationHandle ────────────────────────────────────────────────────

/// Handle returned by [`self_register`].
///
/// Keeps the registry entry alive as long as it is held. Internally, a
/// background task re-registers (renews) the entry every `ttl/3` seconds.
///
/// When dropped, the renewal task is stopped via the cancel oneshot. The
/// registry entry will then expire on its own TTL — allowing the Registry
/// to self-clean without any explicit cancellation RPC.
pub struct SelfRegistrationHandle {
    lease_id: String,
    capability_id: String,
    // Dropping this oneshot signals the renewal task to stop. Without it the
    // spawned task would outlive the handle — dropping a JoinHandle detaches,
    // it does not abort.
    _cancel_tx: oneshot::Sender<()>,
    _renewal_task: JoinHandle<()>,
}

impl SelfRegistrationHandle {
    /// Returns the lease ID granted by Registry for this registration.
    pub fn lease_id(&self) -> &str {
        &self.lease_id
    }

    /// Returns the capability ID assigned by Registry.
    pub fn capability_id(&self) -> &str {
        &self.capability_id
    }
}

/// Register this service in Registry and keep the entry alive.
///
/// Calls `Register` on `registry_client` with the given `interface`, `attrs`,
/// and a `grpc` transport descriptor carrying `host` and `port` — the same
/// convention used by the Go/Java SDK registrations and `ProxyManager`.
///
/// Spawns a background task that re-registers (renews the registry lease)
/// every `ttl_seconds / 3` seconds. Returns a [`SelfRegistrationHandle`]
/// whose `Drop` stops renewal — after which the entry expires on its own TTL.
///
/// Returns `Error` if the initial `Register` call fails so callers can
/// distinguish transient Registry unavailability from permanent failures.
pub async fn self_register(
    mut registry_client: RegistryServiceClient<Channel>,
    interface: &str,
    attrs: HashMap<String, String>,
    host: &str,
    port: u16,
    ttl_seconds: u64,
) -> Result<SelfRegistrationHandle, Error> {
    let transport_config = HashMap::from([
        ("host".to_string(), host.to_string()),
        ("port".to_string(), port.to_string()),
    ]);

    let initial = RegisterRequest {
        interface: interface.to_string(),
        attrs: attrs.clone(),
        ttl_seconds,
        transport: Some(TransportDescriptor {
            r#type: "grpc".to_string(),
            config: transport_config.clone(),
        }),
        capability_id: String::new(),
    };

    let resp = registry_client.register(initial).await?.into_inner();

    let capability_id = resp.capability_id.clone();
    let lease_id = resp
        .lease
        .as_ref()
        .map(|l| l.lease_id.clone())
        .unwrap_or_default();

    info!(
        interface,
        host,
        port,
        capability_id = %capability_id,
        lease_id = %lease_id,
        ttl_seconds,
        "self-registered in Registry"
    );

    // Build the renewal request once; only capability_id carries forward.
    let renewal_request = RegisterRequest {
        interface: interface.to_string(),
        attrs,
        ttl_seconds,
        transport: Some(TransportDescriptor {
            r#type: "grpc".to_string(),
            config: transport_config,
        }),
        capability_id: capability_id.clone(),
    };

    let renewal_interval = Duration::from_secs(ttl_seconds.max(3) / 3);
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
    let log_cap_id = capability_id.clone();

    let renewal_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(renewal_interval);
        interval.tick().await; // consume the immediate tick

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    match registry_client.register(renewal_request.clone()).await {
                        Ok(_) => debug!(capability_id = %log_cap_id, "registry entry renewed"),
                        Err(e) => warn!(capability_id = %log_cap_id, "registry renewal failed: {e}"),
                    }
                }
                _ = &mut cancel_rx => {
                    debug!(capability_id = %log_cap_id, "renewal task stopped");
                    break;
                }
            }
        }
    });

    Ok(SelfRegistrationHandle {
        lease_id,
        capability_id,
        _cancel_tx: cancel_tx,
        _renewal_task: renewal_task,
    })
}

// ── watch_expiry_prefix ───────────────────────────────────────────────────────

/// Subscribe to lease expiry events for a resource-id prefix.
///
/// Opens a `WatchExpiry(resource_id = "")` stream to `lease_client`, filters
/// events client-side to those whose `resource_id` starts with `prefix`, and
/// calls `handler` for each match. If the stream drops (network error, server
/// restart), the loop reconnects automatically with exponential backoff
/// (100 ms → 5 s). On reconnect the LeaseMgr is re-discovered through
/// Registry at `registry_addr`.
///
/// Client-side filtering is currently the only option — the `WatchExpiry`
/// RPC's `resource_id` field is exact-match only. A server-side prefix filter
/// is planned for Phase 1.
///
/// This function runs forever and is intended to be spawned as a `tokio::task`.
/// The task exits only when the process exits.
pub async fn watch_expiry_prefix(
    lease_client: LeaseServiceClient<Channel>,
    registry_addr: String,
    prefix: String,
    handler: impl Fn(ExpiryEvent) + Send + 'static,
) {
    let mut client = lease_client;

    loop {
        match run_watch_expiry_stream(&mut client, &prefix, &handler).await {
            Ok(()) => {
                debug!(prefix, "WatchExpiry stream ended, reconnecting");
            }
            Err(e) => {
                warn!(
                    prefix,
                    "WatchExpiry stream error: {e}, rediscovering LeaseMgr"
                );
                client = retry_forever("watch_expiry_rediscover", || async {
                    discover_lease_mgr(&registry_addr).await
                })
                .await;
            }
        }
    }
}

/// Drive a single WatchExpiry stream until it ends or errors.
async fn run_watch_expiry_stream(
    client: &mut LeaseServiceClient<Channel>,
    prefix: &str,
    handler: &impl Fn(ExpiryEvent),
) -> Result<(), tonic::Status> {
    let mut stream = client
        .watch_expiry(WatchExpiryRequest {
            resource_id: String::new(),
        })
        .await?
        .into_inner();

    while let Some(evt) = stream.message().await? {
        if evt.resource_id.starts_with(prefix) {
            handler(evt);
        }
    }
    Ok(())
}
