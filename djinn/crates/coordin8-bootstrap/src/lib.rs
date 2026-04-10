//! Bootstrap helpers for split-mode Djinn services.
//!
//! This crate sits between `coordin8-core` (no proto dependency) and the
//! service crates. It owns the primitives every service needs when running
//! outside the monolith:
//!
//! - **[`discover_lease_mgr`]** — find LeaseMgr in Registry, retry forever.
//! - **[`self_register`]** — insert a Registry entry and keep it alive via
//!   periodic re-registration, cancel on drop.
//! - **[`watch_expiry_prefix`]** — stream `WatchExpiry` events for a given
//!   resource-id prefix, reconnect automatically.
//! - **[`RemoteLeasing`]** — a `coordin8_core::Leasing` impl that forwards
//!   grant/renew/cancel over gRPC to a discovered LeaseMgr, transparently
//!   re-discovering through Registry on transport failure.
//! - **[`RemoteCapabilityResolver`]** — a `coordin8_core::CapabilityResolver`
//!   impl that forwards template lookups to a Registry gRPC service and
//!   reconnects on transport failure. Used by split-mode Proxy to resolve
//!   templates without a shared in-process `RegistryStore`.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tonic::transport::Channel;
use tracing::{debug, info, warn};

use coordin8_core::{
    CapabilityResolver, Error as CoreError, LeaseRecord, Leasing, RegistryEntry,
    TransportConfig as CoreTransportConfig, TxnEnlister,
};
use coordin8_proto::coordin8::{
    lease_service_client::LeaseServiceClient, registry_service_client::RegistryServiceClient,
    transaction_service_client::TransactionServiceClient, CancelRequest, Capability, EnlistRequest,
    ExpiryEvent, GrantRequest, Lease, LookupRequest, RegisterRequest, RenewRequest,
    TransportDescriptor, WatchExpiryRequest,
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
    let channel = discover_service_channel(registry_addr, "LeaseMgr").await?;
    Ok(LeaseServiceClient::new(channel))
}

/// Look up a service in Registry by `interface` and dial it.
///
/// Helper shared by service-specific discovery functions. Returns a connected
/// tonic `Channel` that the caller wraps in the appropriate generated client.
async fn discover_service_channel(registry_addr: &str, interface: &str) -> Result<Channel, Error> {
    let channel = Channel::from_shared(registry_addr.to_string())
        .map_err(|_| Error::MissingTransport("invalid registry address"))?
        .connect()
        .await?;

    let mut registry = RegistryServiceClient::new(channel);

    let template = HashMap::from([("interface".to_string(), interface.to_string())]);

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

    debug!(addr, interface, "connecting to discovered service");
    Channel::from_shared(addr)
        .map_err(|_| Error::MissingTransport("host/port formed invalid URL"))?
        .connect()
        .await
        .map_err(Error::from)
}

/// Find TxnMgr in Registry and return a connected client. Retries forever with
/// exponential backoff. The discovery template is `interface=TransactionMgr`.
pub async fn discover_txn_mgr(
    registry_addr: &str,
) -> Result<TransactionServiceClient<Channel>, Error> {
    let client = retry_forever("discover_txn_mgr", || async {
        let channel = discover_service_channel(registry_addr, "TransactionMgr").await?;
        Ok::<_, Error>(TransactionServiceClient::new(channel))
    })
    .await;
    info!(registry = registry_addr, "discovered TransactionMgr");
    Ok(client)
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

// ── RemoteLeasing ─────────────────────────────────────────────────────────────

/// A `coordin8_core::Leasing` implementation backed by a gRPC connection to a
/// remote LeaseMgr. This is the Jini lease-as-interface pattern: downstream
/// services hand a `LeaseManager` or a `RemoteLeasing` through the same trait
/// and never know which one they got.
///
/// Failover is transparent: on any transport-like failure (`Unavailable`,
/// `Cancelled`, `Unknown`), the current client is dropped and a fresh one is
/// re-discovered through Registry at `registry_addr`. The in-flight operation
/// is then retried exactly once on the new client. Per-call semantic errors
/// (`NotFound`, `FailedPrecondition`) propagate as typed `coordin8_core::Error`
/// values without a reconnect.
pub struct RemoteLeasing {
    registry_addr: String,
    client: Mutex<LeaseServiceClient<Channel>>,
}

impl RemoteLeasing {
    /// Build a `RemoteLeasing` by first discovering LeaseMgr through Registry.
    ///
    /// Blocks (with exponential backoff) until Registry and LeaseMgr are both
    /// reachable — same retry semantics as [`discover_lease_mgr`].
    pub async fn connect(registry_addr: &str) -> Result<Self, Error> {
        let client = discover_lease_mgr(registry_addr).await?;
        Ok(Self {
            registry_addr: registry_addr.to_string(),
            client: Mutex::new(client),
        })
    }

    /// Re-discover LeaseMgr through Registry and swap in the new client.
    async fn rediscover(&self) -> Result<(), CoreError> {
        let fresh = discover_lease_mgr(&self.registry_addr)
            .await
            .map_err(|e| CoreError::Internal(format!("rediscover failed: {e}")))?;
        *self.client.lock().await = fresh;
        warn!(registry = %self.registry_addr, "RemoteLeasing reconnected to LeaseMgr");
        Ok(())
    }

    /// Invoke `op` against the current client. On transport failure, rediscover
    /// through Registry and retry the call exactly once against the fresh client.
    /// Per-call semantic errors (`NotFound`, `FailedPrecondition`, ...) short-
    /// circuit without a reconnect.
    async fn call_with_retry<F, Fut, T>(&self, fallback_id: &str, op: F) -> Result<T, CoreError>
    where
        F: Fn(LeaseServiceClient<Channel>) -> Fut,
        Fut: Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    {
        let client = self.client.lock().await.clone();
        match op(client).await {
            Ok(resp) => Ok(resp.into_inner()),
            Err(status) if is_transport_failure(&status) => {
                self.rediscover().await?;
                let client = self.client.lock().await.clone();
                op(client)
                    .await
                    .map(tonic::Response::into_inner)
                    .map_err(|s| status_to_core_error(s, fallback_id))
            }
            Err(status) => Err(status_to_core_error(status, fallback_id)),
        }
    }
}

/// Is this gRPC status a transport failure (LeaseMgr likely dead/moved)?
///
/// These codes mean "the RPC itself didn't reach a live server" — the right
/// response is to re-discover and retry. Any other code means the server
/// answered with a semantic result that the caller should see as-is.
fn is_transport_failure(status: &tonic::Status) -> bool {
    use tonic::Code;
    matches!(
        status.code(),
        Code::Unavailable | Code::Cancelled | Code::Unknown | Code::DeadlineExceeded
    )
}

fn status_to_core_error(status: tonic::Status, fallback_id: &str) -> CoreError {
    use tonic::Code;
    match status.code() {
        Code::NotFound => CoreError::LeaseNotFound(fallback_id.to_string()),
        Code::FailedPrecondition => CoreError::LeaseExpired(fallback_id.to_string()),
        _ => CoreError::Internal(format!("{}: {}", status.code(), status.message())),
    }
}

fn proto_to_lease_record(lease: Lease) -> Result<LeaseRecord, CoreError> {
    fn ts_to_dt(
        ts: Option<prost_types::Timestamp>,
    ) -> Result<chrono::DateTime<chrono::Utc>, CoreError> {
        let ts = ts.ok_or_else(|| CoreError::Internal("lease missing timestamp".to_string()))?;
        chrono::DateTime::from_timestamp(ts.seconds, ts.nanos as u32)
            .ok_or_else(|| CoreError::Internal("lease timestamp out of range".to_string()))
    }
    Ok(LeaseRecord {
        lease_id: lease.lease_id,
        resource_id: lease.resource_id,
        granted_at: ts_to_dt(lease.granted_at)?,
        expires_at: ts_to_dt(lease.expires_at)?,
        ttl_seconds: lease.ttl_seconds,
    })
}

#[async_trait]
impl Leasing for RemoteLeasing {
    async fn grant(&self, resource_id: &str, ttl_secs: u64) -> Result<LeaseRecord, CoreError> {
        let request = GrantRequest {
            resource_id: resource_id.to_string(),
            ttl_seconds: ttl_secs,
        };
        let lease = self
            .call_with_retry(resource_id, |mut c| {
                let req = request.clone();
                async move { c.grant(req).await }
            })
            .await?;
        proto_to_lease_record(lease)
    }

    async fn renew(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRecord, CoreError> {
        let request = RenewRequest {
            lease_id: lease_id.to_string(),
            ttl_seconds: ttl_secs,
        };
        let lease = self
            .call_with_retry(lease_id, |mut c| {
                let req = request.clone();
                async move { c.renew(req).await }
            })
            .await?;
        proto_to_lease_record(lease)
    }

    async fn cancel(&self, lease_id: &str) -> Result<(), CoreError> {
        let request = CancelRequest {
            lease_id: lease_id.to_string(),
        };
        self.call_with_retry(lease_id, |mut c| {
            let req = request.clone();
            async move { c.cancel(req).await }
        })
        .await
        .map(|_| ())
    }
}

// ── RemoteCapabilityResolver ─────────────────────────────────────────────────

/// A `coordin8_core::CapabilityResolver` implementation backed by a Registry
/// gRPC client. Split-mode Proxy uses this to resolve templates without a
/// shared in-process `RegistryStore` — the matching happens on the Registry
/// server via its `Lookup` RPC.
///
/// On transport failure the Registry connection is rebuilt against
/// `registry_addr` and the lookup is retried once. A `NotFound` status is
/// treated as a successful "no match" (returns `Ok(None)`), since Registry
/// reports "no capability matches this template" via `Status::not_found`.
pub struct RemoteCapabilityResolver {
    registry_addr: String,
    client: Mutex<RegistryServiceClient<Channel>>,
}

impl RemoteCapabilityResolver {
    /// Build a `RemoteCapabilityResolver` by dialing the Registry at
    /// `registry_addr`. Retries forever with exponential backoff until the
    /// Registry is reachable.
    pub async fn connect(registry_addr: &str) -> Result<Self, Error> {
        let client = retry_forever("registry_dial", || async {
            RegistryServiceClient::connect(registry_addr.to_string())
                .await
                .map_err(Error::from)
        })
        .await;
        Ok(Self {
            registry_addr: registry_addr.to_string(),
            client: Mutex::new(client),
        })
    }

    async fn reconnect(&self) -> Result<(), CoreError> {
        let fresh = RegistryServiceClient::connect(self.registry_addr.clone())
            .await
            .map_err(|e| CoreError::Internal(format!("registry reconnect failed: {e}")))?;
        *self.client.lock().await = fresh;
        warn!(registry = %self.registry_addr, "RemoteCapabilityResolver reconnected to Registry");
        Ok(())
    }

    /// Invoke `op` against the current Registry client. On transport failure,
    /// reconnect and retry exactly once. Mirrors `RemoteLeasing::call_with_retry`
    /// — the client is cloned out of the Mutex before the RPC, so the lock is
    /// never held across `.await`.
    /// Invoke Registry::Lookup against the current client. On transport failure,
    /// reconnect and retry exactly once. Mirrors `RemoteLeasing::call_with_retry`
    /// — the client is cloned out of the Mutex before the RPC, so the lock is
    /// never held across `.await`.
    async fn lookup_with_retry(
        &self,
        request: LookupRequest,
    ) -> Result<tonic::Response<Capability>, tonic::Status> {
        let mut client = self.client.lock().await.clone();
        match client.lookup(request.clone()).await {
            Ok(resp) => Ok(resp),
            Err(status) if is_transport_failure(&status) => {
                if let Err(e) = self.reconnect().await {
                    return Err(tonic::Status::unavailable(format!("{e}")));
                }
                let mut client = self.client.lock().await.clone();
                client.lookup(request).await
            }
            Err(status) => Err(status),
        }
    }
}

fn capability_to_registry_entry(cap: Capability) -> RegistryEntry {
    RegistryEntry {
        capability_id: cap.capability_id,
        // Registry::Lookup does not return lease_id on the wire, and Proxy
        // only reads the transport; leave this empty rather than invent one.
        lease_id: String::new(),
        interface: cap.interface,
        attrs: cap.attrs,
        transport: cap.transport.map(|t| CoreTransportConfig {
            transport_type: t.r#type,
            config: t.config,
        }),
    }
}

#[async_trait]
impl CapabilityResolver for RemoteCapabilityResolver {
    async fn resolve(
        &self,
        template: &HashMap<String, String>,
    ) -> Result<Option<RegistryEntry>, CoreError> {
        let request = LookupRequest {
            template: template.clone(),
        };

        match self.lookup_with_retry(request).await {
            Ok(resp) => Ok(Some(capability_to_registry_entry(resp.into_inner()))),
            Err(status) if status.code() == tonic::Code::NotFound => Ok(None),
            Err(status) => Err(CoreError::Internal(format!(
                "registry lookup failed: {status}"
            ))),
        }
    }
}

// ── RemoteTxnEnlister ────────────────────────────────────────────────────────

/// A `coordin8_core::TxnEnlister` backed by a gRPC connection to a remote
/// TransactionMgr. Used by split-mode services (Space today, others later) so
/// they can auto-enlist as 2PC participants without taking a direct dependency
/// on `coordin8-txn`.
///
/// Discovery is **lazy**: `new()` does no I/O, so Space can boot before TxnMgr
/// exists. The first `enlist()` call drives discovery through Registry, caches
/// the resulting client, and retries forever until TxnMgr is reachable —
/// matching the "absence is a signal" stance from the design napkin. On any
/// later transport failure the cached client is dropped and rediscovered.
pub struct RemoteTxnEnlister {
    registry_addr: String,
    client: Mutex<Option<TransactionServiceClient<Channel>>>,
}

impl RemoteTxnEnlister {
    /// Build a lazy `RemoteTxnEnlister`. Does not touch the network — the
    /// first `enlist` call will discover TxnMgr through Registry.
    pub fn new(registry_addr: &str) -> Self {
        Self {
            registry_addr: registry_addr.to_string(),
            client: Mutex::new(None),
        }
    }

    /// Return a ready client, discovering TxnMgr if we don't have one yet.
    /// Blocks (with exponential backoff) until Registry and TxnMgr are both
    /// reachable.
    async fn ensure_client(&self) -> Result<TransactionServiceClient<Channel>, CoreError> {
        if let Some(c) = self.client.lock().await.as_ref() {
            return Ok(c.clone());
        }
        let fresh = discover_txn_mgr(&self.registry_addr)
            .await
            .map_err(|e| CoreError::Internal(format!("discover txn_mgr failed: {e}")))?;
        let mut guard = self.client.lock().await;
        *guard = Some(fresh.clone());
        Ok(fresh)
    }

    async fn rediscover(&self) -> Result<TransactionServiceClient<Channel>, CoreError> {
        let fresh = discover_txn_mgr(&self.registry_addr)
            .await
            .map_err(|e| CoreError::Internal(format!("rediscover txn_mgr failed: {e}")))?;
        *self.client.lock().await = Some(fresh.clone());
        warn!(registry = %self.registry_addr, "RemoteTxnEnlister reconnected to TxnMgr");
        Ok(fresh)
    }
}

#[async_trait]
impl TxnEnlister for RemoteTxnEnlister {
    async fn enlist(&self, txn_id: &str, endpoint: &str) -> Result<(), CoreError> {
        let request = EnlistRequest {
            txn_id: txn_id.to_string(),
            participant_endpoint: endpoint.to_string(),
            crash_count: 0,
        };

        let mut client = self.ensure_client().await?;
        match client.enlist(request.clone()).await {
            Ok(_) => Ok(()),
            Err(status) if is_transport_failure(&status) => {
                let mut client = self.rediscover().await?;
                client
                    .enlist(request)
                    .await
                    .map(|_| ())
                    .map_err(|s| CoreError::Internal(format!("enlist failed: {s}")))
            }
            Err(status) => Err(CoreError::Internal(format!("enlist failed: {status}"))),
        }
    }
}
