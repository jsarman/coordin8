//! Bootstrap helpers for split-mode Djinn services.
//!
//! This crate sits between `coordin8-core` (no proto dependency) and the
//! service crates. It owns the primitives every service needs when running
//! outside the monolith:
//!
//! - **[`self_register`]** — insert a Registry entry and keep it alive via
//!   periodic re-registration, cancel on drop.
//! - **[`RemoteCapabilityResolver`]** — a `coordin8_core::CapabilityResolver`
//!   impl that forwards template lookups to a Registry gRPC service and
//!   reconnects on transport failure. Used by split-mode Proxy to resolve
//!   templates without a shared in-process `RegistryStore`.
//! - **[`discover_txn_mgr`]** / **[`RemoteTxnEnlister`]** — find TransactionMgr
//!   in Registry and enlist as a 2PC participant, lazily and with transparent
//!   reconnect.
//! - **[`PendingCapabilityResolver`]** — lets Proxy start serving immediately
//!   on `NotServing` health while it resolves its Registry dependency in the
//!   background, rather than blocking its whole gRPC serve loop.
//!
//! Leasing is *not* here. Each service that grants leased resources
//! (Registry, Space, EventMgr, TransactionMgr) embeds its own
//! `coordin8_lease::LeaseManager` in-process — matching Jini/Apache River's
//! `Landlord` pattern, where every grantor manages its own leases rather than
//! depending on a shared external service. See
//! `.claude/plans/distributed-leasing/PRD.md`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use coordin8_auth::{wrap_channel, AuthedChannel, ClientAuthConfig};
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;
use tonic::transport::Channel;
use tracing::{debug, info, warn};

use coordin8_core::{
    CapabilityResolver, Error as CoreError, RegistryEntry, TransportConfig as CoreTransportConfig,
    TxnEnlister,
};
use coordin8_proto::coordin8::{
    registry_service_client::RegistryServiceClient,
    transaction_service_client::TransactionServiceClient, Capability, EnlistRequest, LookupRequest,
    RegisterRequest, TransportDescriptor,
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

/// Look up a service in Registry by `interface` and dial it.
///
/// Helper shared by service-specific discovery functions. Returns a connected
/// tonic `Channel` that the caller wraps in the appropriate generated client.
/// The `Lookup` call this makes against Registry is itself an internal call,
/// so it goes through `client_auth` too.
async fn discover_service_channel(
    registry_addr: &str,
    interface: &str,
    client_auth: &ClientAuthConfig,
) -> Result<Channel, Error> {
    let channel = Channel::from_shared(registry_addr.to_string())
        .map_err(|_| Error::MissingTransport("invalid registry address"))?
        .connect()
        .await?;

    let mut registry = RegistryServiceClient::with_interceptor(channel, client_auth.interceptor());

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
    client_auth: &ClientAuthConfig,
) -> Result<TransactionServiceClient<AuthedChannel>, Error> {
    let client = retry_forever("discover_txn_mgr", || async {
        let channel =
            discover_service_channel(registry_addr, "TransactionMgr", client_auth).await?;
        Ok::<_, Error>(TransactionServiceClient::new(wrap_channel(
            channel,
            client_auth,
        )))
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
    mut registry_client: RegistryServiceClient<AuthedChannel>,
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
    client_auth: ClientAuthConfig,
    client: Mutex<RegistryServiceClient<AuthedChannel>>,
}

impl RemoteCapabilityResolver {
    /// Build a `RemoteCapabilityResolver` by dialing the Registry at
    /// `registry_addr`. Retries forever with exponential backoff until the
    /// Registry is reachable. `client_auth` (Decision 8) controls what, if
    /// anything, this resolver attaches to its own outbound `Lookup` calls.
    pub async fn connect(
        registry_addr: &str,
        client_auth: ClientAuthConfig,
    ) -> Result<Self, Error> {
        let client = retry_forever("registry_dial", || async {
            let channel = Channel::from_shared(registry_addr.to_string())
                .map_err(|_| Error::MissingTransport("invalid registry address"))?
                .connect()
                .await?;
            Ok::<_, Error>(RegistryServiceClient::new(wrap_channel(
                channel,
                &client_auth,
            )))
        })
        .await;
        Ok(Self {
            registry_addr: registry_addr.to_string(),
            client_auth,
            client: Mutex::new(client),
        })
    }

    async fn reconnect(&self) -> Result<(), CoreError> {
        let channel = Channel::from_shared(self.registry_addr.clone())
            .map_err(|_| CoreError::Internal("invalid registry address".to_string()))?
            .connect()
            .await
            .map_err(|e| CoreError::Internal(format!("registry reconnect failed: {e}")))?;
        let fresh = RegistryServiceClient::new(wrap_channel(channel, &self.client_auth));
        *self.client.lock().await = fresh;
        warn!(registry = %self.registry_addr, "RemoteCapabilityResolver reconnected to Registry");
        Ok(())
    }

    /// Invoke Registry::Lookup against the current client. On transport
    /// failure, reconnect and retry exactly once. The client is cloned out
    /// of the Mutex before the RPC, so the lock is never held across `.await`.
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

/// Is this gRPC status a transport failure (peer likely dead/moved)?
///
/// These codes mean "the RPC itself didn't reach a live server" — the right
/// response is to re-discover/reconnect and retry. Any other code means the
/// server answered with a semantic result that the caller should see as-is.
fn is_transport_failure(status: &tonic::Status) -> bool {
    use tonic::Code;
    matches!(
        status.code(),
        Code::Unavailable | Code::Cancelled | Code::Unknown | Code::DeadlineExceeded
    )
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
    client_auth: ClientAuthConfig,
    client: Mutex<Option<TransactionServiceClient<AuthedChannel>>>,
}

impl RemoteTxnEnlister {
    /// Build a lazy `RemoteTxnEnlister`. Does not touch the network — the
    /// first `enlist` call will discover TxnMgr through Registry.
    /// `client_auth` (Decision 8) controls what, if anything, this enlister
    /// attaches to its own outbound `Enlist` calls against TxnMgr.
    pub fn new(registry_addr: &str, client_auth: ClientAuthConfig) -> Self {
        Self {
            registry_addr: registry_addr.to_string(),
            client_auth,
            client: Mutex::new(None),
        }
    }

    /// Return a ready client, discovering TxnMgr if we don't have one yet.
    /// Blocks (with exponential backoff) until Registry and TxnMgr are both
    /// reachable.
    async fn ensure_client(&self) -> Result<TransactionServiceClient<AuthedChannel>, CoreError> {
        if let Some(c) = self.client.lock().await.as_ref() {
            return Ok(c.clone());
        }
        let fresh = discover_txn_mgr(&self.registry_addr, &self.client_auth)
            .await
            .map_err(|e| CoreError::Internal(format!("discover txn_mgr failed: {e}")))?;
        let mut guard = self.client.lock().await;
        *guard = Some(fresh.clone());
        Ok(fresh)
    }

    async fn rediscover(&self) -> Result<TransactionServiceClient<AuthedChannel>, CoreError> {
        let fresh = discover_txn_mgr(&self.registry_addr, &self.client_auth)
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

// ── PendingCapabilityResolver ─────────────────────────────────────────────────
//
// Proxy shouldn't have to block its entire gRPC serve loop behind Registry
// discovery before it can start accepting connections at all — that's the
// boot-order gap this closes. Constructible immediately, so the manager
// built around it (and therefore the server) can start right away. Every
// call fails fast with `Error::Unavailable` until `install()` is called by a
// background discovery task, never hanging the caller.

/// A [`CapabilityResolver`] impl that starts unresolved and becomes ready
/// once a background task calls [`install`](Self::install) with a real
/// [`RemoteCapabilityResolver`]. See the module note above.
pub struct PendingCapabilityResolver {
    inner: RwLock<Option<Arc<RemoteCapabilityResolver>>>,
}

impl Default for PendingCapabilityResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingCapabilityResolver {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    /// Install the resolved dependency. Called once, by the background
    /// discovery task, after which every call delegates to it.
    pub async fn install(&self, resolved: RemoteCapabilityResolver) {
        *self.inner.write().await = Some(Arc::new(resolved));
    }
}

#[async_trait]
impl CapabilityResolver for PendingCapabilityResolver {
    async fn resolve(
        &self,
        template: &HashMap<String, String>,
    ) -> Result<Option<RegistryEntry>, CoreError> {
        match self.inner.read().await.as_ref() {
            Some(resolver) => resolver.resolve(template).await,
            None => Err(CoreError::Unavailable(
                "waiting on dependency: Registry".to_string(),
            )),
        }
    }
}
