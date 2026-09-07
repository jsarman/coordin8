//! Djinn service boot functions.
//!
//! Each function binds a gRPC server and blocks until the listener closes.
//! Use `tokio::spawn` in tests to run multiple services concurrently.
//!
//! Leasing is distributed, not centralized: Registry, Space, EventMgr, and
//! TransactionMgr each embed their own `LeaseManager` and mount `LeaseService`
//! on their own port, matching Jini/Apache River's `Landlord` pattern — see
//! `.claude/plans/distributed-leasing/PRD.md`. There is no standalone
//! LeaseMgr process; a lease holder renews against whichever service granted
//! it, using the `grantor_host`/`grantor_port` carried on the `Lease` itself.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::broadcast;
use tonic::transport::Server;
use tracing::info;

use coordin8_auth::AuthConfig;
use coordin8_bootstrap::{self_register, RemoteCapabilityResolver, RemoteTxnEnlister};
use coordin8_core::{
    EventStore, LeaseReclaimed, LeaseStore, Leasing, RegistryStore, SpaceStore, TxnStore,
};
use coordin8_event::{EventManager, EventServiceImpl};
use coordin8_lease::{LeaseManager, LeaseServiceImpl};
use coordin8_proto::coordin8::event_service_server::EventServiceServer;
use coordin8_proto::coordin8::lease_service_server::LeaseServiceServer;
use coordin8_proto::coordin8::participant_service_server::ParticipantServiceServer;
use coordin8_proto::coordin8::proxy_service_server::ProxyServiceServer;
use coordin8_proto::coordin8::registry_service_server::RegistryServiceServer;
use coordin8_proto::coordin8::space_service_server::SpaceServiceServer;
use coordin8_proto::coordin8::transaction_service_server::TransactionServiceServer;
use coordin8_provider_local::{
    InMemoryEventStore, InMemoryLeaseStore, InMemoryRegistryStore, InMemorySpaceStore,
    InMemoryTxnStore,
};
use coordin8_proxy::{LocalCapabilityResolver, ProxyConfig, ProxyManager, ProxyServiceImpl};
use coordin8_registry::service::RegistryBroadcast;
use coordin8_registry::{store::RegistryIndex, RegistryServiceImpl};
use coordin8_space::{SpaceManager, SpaceParticipantService, SpaceServiceImpl};
use coordin8_txn::{LocalTxnEnlister, TxnManager, TxnServiceImpl};
use tonic_health::ServingStatus;

// ── Env var helpers (pub for tests) ──────────────────────────────────────────

/// Read `COORDIN8_BIND_ADDR`, defaulting to `0.0.0.0:0` (OS-assigned port).
pub fn bind_addr() -> String {
    std::env::var("COORDIN8_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:0".to_string())
}

/// Read `COORDIN8_ADVERTISE_HOST`, defaulting to `127.0.0.1`.
///
/// The port registered with Registry always comes from the bound listener.
/// Override this in Docker so peers dial the container name. Mirrors the
/// `ADVERTISE_HOST` convention used by the Go example services.
pub fn advertise_host() -> String {
    std::env::var("COORDIN8_ADVERTISE_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// Dial Registry at `registry_url`, retrying with a fixed backoff, and wrap
/// the client with `client_auth` (Decision 8 —
/// `.claude/plans/grpc-security/PRD.md`) plus trace-context propagation
/// (Decision 2 — `.claude/plans/observability/PRD.md`) so every internal
/// self-registration call attaches whatever the auth strategy provides
/// (nothing, by default) and carries the current trace onward.
async fn dial_registry_authed(
    registry_url: &str,
    client_auth: &coordin8_auth::ClientAuthConfig,
) -> coordin8_proto::coordin8::registry_service_client::RegistryServiceClient<
    coordin8_observability::TracedAuthedChannel,
> {
    loop {
        match tonic::transport::Channel::from_shared(registry_url.to_string())
            .expect("registry_url is always a valid URL by this point")
            .connect()
            .await
        {
            Ok(channel) => {
                return coordin8_proto::coordin8::registry_service_client::RegistryServiceClient::new(
                    coordin8_observability::wrap_traced_channel(channel, client_auth),
                )
            }
            Err(e) => {
                tracing::warn!("registry dial ({registry_url}) failed: {e}, retrying in 200ms");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// Dials Registry and calls [`self_register`], retrying the whole pair with
/// a fixed backoff until it succeeds. `dial_registry_authed` already retries
/// connection failures forever; this closes the remaining gap the initial
/// `Register` RPC itself could fail even once connected (Registry
/// transiently erroring) — previously terminal, since the caller just
/// logged and gave up rather than retrying, unlike the dial it wraps.
async fn self_register_retrying(
    registry_url: &str,
    client_auth: &coordin8_auth::ClientAuthConfig,
    interface: &str,
    attrs: std::collections::HashMap<String, String>,
    host: &str,
    port: u16,
    ttl_seconds: u64,
) -> coordin8_bootstrap::SelfRegistrationHandle {
    loop {
        let registry_client = dial_registry_authed(registry_url, client_auth).await;
        match self_register(
            registry_client,
            interface,
            attrs.clone(),
            host,
            port,
            ttl_seconds,
        )
        .await
        {
            Ok(handle) => return handle,
            Err(e) => {
                tracing::warn!("{interface}: self_register failed: {e}, retrying in 500ms");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Self-register a bundled-mode service into the same process's Registry.
///
/// Split mode already self-registers every service (see the `run_*_on_listener`
/// functions); bundled mode never did, since a client could always assume
/// "one host, fixed ports" and skip Registry entirely for the core services.
/// That assumption is what the registry-bootstrap effort
/// (`.claude/plans/registry-bootstrap/`) removes — clients should look up
/// Space/EventMgr/Proxy/TransactionMgr through Registry the same way whether
/// bundled or split. This makes that true for bundled mode too.
///
/// Retries the initial dial (Registry's own server task may not have started
/// accepting yet) and then holds the registration alive for the process
/// lifetime — matching the split-mode self-registration futures' pattern.
fn spawn_bundled_self_register(interface: &'static str, port: u16, client_auth: AuthConfig) {
    let advertise = advertise_host();
    let client_auth = client_auth.client_config(interface);
    tokio::spawn(async move {
        let handle = self_register_retrying(
            "http://localhost:9002",
            &client_auth,
            interface,
            std::collections::HashMap::new(),
            &advertise,
            port,
            30,
        )
        .await;
        info!(
            "  ✓ {interface}: self-registered (capability: {}, lease: {})",
            handle.initial_capability_id(),
            handle.initial_lease_id()
        );
        std::future::pending::<()>().await;
    });
}

// ── Provider selection (pub(crate) — shared by run_all() and every split-mode
//    function, so COORDIN8_PROVIDER=dynamo works the same in both) ───────────

/// Read `COORDIN8_PROVIDER`, defaulting to `"local"`.
fn provider_from_env() -> String {
    std::env::var("COORDIN8_PROVIDER").unwrap_or_else(|_| "local".into())
}

/// `namespace` distinguishes each service's own lease store when using
/// DynamoDB (e.g. `"space"` → table `coordin8_leases_space`), so Registry,
/// Space, EventMgr, and TransactionMgr never share lease state even though
/// they all use the same `LeaseStore` trait and, for the in-memory provider,
/// the same concrete type.
async fn lease_store_from_env(namespace: &str) -> Result<Arc<dyn LeaseStore>> {
    Ok(match provider_from_env().as_str() {
        "dynamo" => {
            let client = coordin8_provider_dynamo::make_dynamo_client().await;
            let table_name = format!("coordin8_leases_{namespace}");
            let store = Arc::new(coordin8_provider_dynamo::DynamoLeaseStore::with_table(
                client, table_name,
            ));
            store.init().await?;
            info!("  ✓ Provider: dynamo (DynamoDB) — LeaseStore ({namespace})");
            store
        }
        _ => {
            info!("  ✓ Provider: local (in-memory) — LeaseStore ({namespace})");
            Arc::new(InMemoryLeaseStore::new())
        }
    })
}

async fn registry_store_from_env() -> Result<Arc<dyn RegistryStore>> {
    Ok(match provider_from_env().as_str() {
        "dynamo" => {
            let client = coordin8_provider_dynamo::make_dynamo_client().await;
            let store = Arc::new(coordin8_provider_dynamo::DynamoRegistryStore::new(client));
            store.init().await?;
            info!("  ✓ Provider: dynamo (DynamoDB) — RegistryStore");
            store
        }
        _ => {
            info!("  ✓ Provider: local (in-memory) — RegistryStore");
            Arc::new(InMemoryRegistryStore::new())
        }
    })
}

async fn event_store_from_env() -> Result<Arc<dyn EventStore>> {
    Ok(match provider_from_env().as_str() {
        "dynamo" => {
            let client = coordin8_provider_dynamo::make_dynamo_client().await;
            let store = Arc::new(coordin8_provider_dynamo::DynamoEventStore::new(client));
            store.init().await?;
            info!("  ✓ Provider: dynamo (DynamoDB) — EventStore");
            store
        }
        _ => {
            info!("  ✓ Provider: local (in-memory) — EventStore");
            Arc::new(InMemoryEventStore::new())
        }
    })
}

async fn txn_store_from_env() -> Result<Arc<dyn TxnStore>> {
    Ok(match provider_from_env().as_str() {
        "dynamo" => {
            let client = coordin8_provider_dynamo::make_dynamo_client().await;
            let store = Arc::new(coordin8_provider_dynamo::DynamoTxnStore::new(client));
            store.init().await?;
            info!("  ✓ Provider: dynamo (DynamoDB) — TxnStore");
            store
        }
        _ => {
            info!("  ✓ Provider: local (in-memory) — TxnStore");
            Arc::new(InMemoryTxnStore::new())
        }
    })
}

async fn space_store_from_env() -> Result<Arc<dyn SpaceStore>> {
    Ok(match provider_from_env().as_str() {
        "dynamo" => {
            let client = coordin8_provider_dynamo::make_dynamo_client().await;
            let store = Arc::new(coordin8_provider_dynamo::DynamoSpaceStore::new(client));
            store.init().await?;
            info!("  ✓ Provider: dynamo (DynamoDB) — SpaceStore");
            store
        }
        _ => {
            info!("  ✓ Provider: local (in-memory) — SpaceStore");
            Arc::new(InMemorySpaceStore::new())
        }
    })
}

// ── Embedded Landlord (pub(crate) — every leasing service builds one) ───────

/// A generated gRPC server type wrapped with [`AuthConfig`]'s interceptor —
/// the concrete type every service's own primary/lease server ends up as
/// once auth is wired in (Decision 3 — `.claude/plans/grpc-security/PRD.md`).
type Authed<S> = tonic::service::interceptor::InterceptedService<S, AuthConfig>;

/// Build an embedded Landlord for a service that grants leased resources:
/// its own `LeaseManager`, its own reaper task, and the `LeaseService` gRPC
/// glue ready to mount on that service's own server. `grantor_host`/
/// `grantor_port` are stamped onto every `Lease` this manager grants, so a
/// holder always knows where to renew (see `Lease`'s proto doc comment).
///
/// Every service that grants leased resources — Registry, Space, EventMgr,
/// TransactionMgr — calls this instead of dialing a shared external LeaseMgr.
/// This is Jini's `Landlord` pattern: each grantor manages its own leases
/// in-process. See `.claude/plans/distributed-leasing/PRD.md`.
async fn embedded_landlord(
    namespace: &str,
    grantor_host: &str,
    grantor_port: u16,
    auth_config: &AuthConfig,
) -> Result<(
    Arc<LeaseManager>,
    Authed<LeaseServiceServer<LeaseServiceImpl>>,
)> {
    let store = lease_store_from_env(namespace).await?;
    let config = coordin8_core::LeaseConfig::from_env_for(Some(namespace));
    info!(
        "  Lease policy ({namespace}): max_ttl={}, preferred_ttl={}s",
        config
            .max_ttl
            .map_or("FOREVER".to_string(), |v| format!("{}s", v)),
        config.preferred_ttl
    );

    let (expiry_tx, _) = broadcast::channel::<LeaseReclaimed>(256);
    let manager = Arc::new(LeaseManager::new(store, config, expiry_tx.clone()));

    let reaper_manager = Arc::clone(&manager);
    let reaper_tx = expiry_tx.clone();
    tokio::spawn(async move {
        coordin8_lease::reaper::run_reaper(reaper_manager, reaper_tx, Duration::from_secs(1)).await;
    });

    let svc = LeaseServiceServer::with_interceptor(
        LeaseServiceImpl::new(Arc::clone(&manager), expiry_tx, grantor_host, grantor_port),
        auth_config.clone(),
    );

    Ok((manager, svc))
}

/// Subscribe to a `LeaseManager`'s own reclaim broadcast (expiry or explicit
/// cancel — see `LeaseManager::cancel`) and dispatch each event to `handler`.
///
/// Explicitly handles `RecvError::Lagged` by logging and continuing.
/// Treating it as a stream close (the old `while let Ok(..)` shape every
/// cascade handler used to have) silently and permanently killed the cascade
/// the first time a burst of reclaims outran the channel's buffer — e.g. a
/// burst of same-TTL tuple writes on Space, previously the worst-hit case
/// since every service shared one channel. Each service now has its own
/// private channel with only its own traffic, which lowers the odds of a
/// lag burst, but the fix matters regardless: a task that silently stops
/// forever is a much worse failure mode than occasionally missing an
/// individual cleanup during a burst.
fn spawn_cascade(
    label: &'static str,
    mut rx: broadcast::Receiver<LeaseReclaimed>,
    handler: impl Fn(LeaseReclaimed) + Send + 'static,
) {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => handler(event),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "{label}: cascade lagged, missed {n} reclaim event(s) — \
                         some resources may not have been cleaned up promptly"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::warn!("{label}: cascade channel closed, stopping");
                    break;
                }
            }
        }
    });
}

// ── Monolith boot ─────────────────────────────────────────────────────────────

/// Boot every service in a single process on fixed ports (the original monolith).
pub async fn run_all() -> Result<()> {
    info!("Djinn starting...");

    // ── Layer 0: Provider ────────────────────────────────────────────────────
    let registry_store = registry_store_from_env().await?;
    let event_store = event_store_from_env().await?;
    let txn_store = txn_store_from_env().await?;
    let space_store = space_store_from_env().await?;

    let host = advertise_host();

    // gRPC auth is opt-in and off unless COORDIN8_JWT_SECRET is set — see
    // .claude/plans/grpc-security/PRD.md Decision 6. One shared config for
    // the whole bundled process.
    let auth_config = AuthConfig::from_env();
    if auth_config.enabled() {
        info!("  gRPC auth: enabled");
    }

    // ── Registry ─────────────────────────────────────────────────────────────
    let (registry_lease_manager, lease_svc_for_registry) =
        embedded_landlord("registry", &host, 9002, &auth_config).await?;
    let (registry_tx, _): (RegistryBroadcast, _) = broadcast::channel(256);
    let registry_index = Arc::new(RegistryIndex::new(registry_store.clone()));

    {
        let registry_expiry_index = Arc::clone(&registry_index);
        let registry_expiry_tx = registry_tx.clone();
        spawn_cascade(
            "registry",
            registry_lease_manager.expiry_tx().subscribe(),
            move |LeaseReclaimed { record, .. }| {
                let index = Arc::clone(&registry_expiry_index);
                let tx = registry_expiry_tx.clone();
                tokio::spawn(async move {
                    if let Ok(Some(entry)) = index.unregister_by_lease(&record.lease_id).await {
                        tracing::debug!(
                            capability_id = %entry.capability_id,
                            interface = %entry.interface,
                            lease_id = %record.lease_id,
                            "registry entry reclaimed"
                        );
                        let _ = tx.send(coordin8_registry::service::RegistryChangedEvent {
                            event_type: 1,
                            entry,
                        });
                    }
                });
            },
        );
    }
    info!("  ✓ Registry: ready");

    // ── EventMgr ─────────────────────────────────────────────────────────────
    let (event_lease_manager, lease_svc_for_event) =
        embedded_landlord("event", &host, 9005, &auth_config).await?;
    let event_leasing: Arc<dyn Leasing> = event_lease_manager.clone();
    let (event_tx, _) = broadcast::channel::<coordin8_core::EventRecord>(256);
    let event_manager = Arc::new(EventManager::new(event_store, event_leasing, event_tx));

    {
        let event_expiry_mgr = Arc::clone(&event_manager);
        spawn_cascade(
            "event",
            event_lease_manager.expiry_tx().subscribe(),
            move |LeaseReclaimed { record, .. }| {
                let mgr = Arc::clone(&event_expiry_mgr);
                tokio::spawn(async move {
                    let _ = mgr.unsubscribe_by_lease(&record.lease_id).await;
                });
            },
        );
    }
    info!("  ✓ EventMgr: ready");

    // ── Proxy ────────────────────────────────────────────────────────────────
    // Proxy never grants leases — it only resolves capability templates
    // against Registry, unrelated to the leasing model.
    let proxy_config = ProxyConfig::from_env();
    let proxy_resolver = Arc::new(LocalCapabilityResolver::new(registry_store));
    let proxy_manager = Arc::new(ProxyManager::new(proxy_resolver, proxy_config));
    info!("  ✓ Proxy: ready");

    // ── TransactionMgr ───────────────────────────────────────────────────────
    let (txn_lease_manager, lease_svc_for_txn) =
        embedded_landlord("txn", &host, 9004, &auth_config).await?;
    let txn_leasing: Arc<dyn Leasing> = txn_lease_manager.clone();
    let txn_manager = Arc::new(TxnManager::with_client_auth(
        txn_store,
        txn_leasing,
        auth_config.client_config("txn"),
    ));

    {
        let txn_expiry_mgr = Arc::clone(&txn_manager);
        spawn_cascade(
            "txn",
            txn_lease_manager.expiry_tx().subscribe(),
            move |LeaseReclaimed { record, .. }| {
                if let Some(txn_id) = record.resource_id.strip_prefix("txn:") {
                    let mgr = Arc::clone(&txn_expiry_mgr);
                    let txn_id = txn_id.to_string();
                    tokio::spawn(async move {
                        let _ = mgr.abort_expired(&txn_id).await;
                    });
                }
            },
        );
    }
    info!("  ✓ TransactionMgr: ready");

    // ── Space ────────────────────────────────────────────────────────────────
    let (space_lease_manager, lease_svc_for_space) =
        embedded_landlord("space", &host, 9006, &auth_config).await?;
    let space_leasing: Arc<dyn Leasing> = space_lease_manager.clone();
    let (space_tuple_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    let (space_expiry_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    // Bundled mode: auto-enlist goes straight into the local TxnManager so the
    // 2PC coordinator dials back into our own space participant on :9006.
    let space_enlister = Arc::new(LocalTxnEnlister::new(Arc::clone(&txn_manager)));
    let space_participant_endpoint = format!("{host}:9006");
    let space_manager = Arc::new(SpaceManager::with_enlister(
        space_store,
        space_leasing,
        space_tuple_tx,
        space_expiry_tx,
        space_enlister,
        space_participant_endpoint,
    ));

    {
        let space_expiry_mgr = Arc::clone(&space_manager);
        spawn_cascade(
            "space",
            space_lease_manager.expiry_tx().subscribe(),
            move |LeaseReclaimed { record, .. }| {
                let mgr = Arc::clone(&space_expiry_mgr);
                tokio::spawn(async move {
                    if record.resource_id.starts_with("space:") {
                        mgr.on_tuple_expired(&record.lease_id).await;
                    } else if record.resource_id.starts_with("space-watch:") {
                        mgr.on_watch_expired(&record.lease_id).await;
                    }
                });
            },
        );
    }
    info!("  ✓ Space: ready");

    // ── gRPC servers ─────────────────────────────────────────────────────────
    let registry_addr = "0.0.0.0:9002".parse()?;
    let proxy_addr = "0.0.0.0:9003".parse()?;
    let txn_addr = "0.0.0.0:9004".parse()?;
    let event_addr = "0.0.0.0:9005".parse()?;
    let space_addr = "0.0.0.0:9006".parse()?;

    let registry_leasing: Arc<dyn Leasing> = registry_lease_manager;
    let registry_svc = RegistryServiceServer::with_interceptor(
        RegistryServiceImpl::new(registry_index, registry_leasing, registry_tx, &host, 9002),
        auth_config.clone(),
    );
    let proxy_svc = ProxyServiceServer::with_interceptor(
        ProxyServiceImpl::new(proxy_manager),
        auth_config.clone(),
    );
    let txn_svc = TransactionServiceServer::with_interceptor(
        TxnServiceImpl::new(txn_manager, &host, 9004),
        auth_config.clone(),
    );
    let event_svc = EventServiceServer::with_interceptor(
        EventServiceImpl::new(event_manager, &host, 9005),
        auth_config.clone(),
    );
    let space_svc = SpaceServiceServer::with_interceptor(
        SpaceServiceImpl::new(Arc::clone(&space_manager), &host, 9006),
        auth_config.clone(),
    );
    let space_participant_svc = ParticipantServiceServer::with_interceptor(
        SpaceParticipantService::new(space_manager),
        auth_config.clone(),
    );

    info!(
        "  ✓ Registry:       listening on {} (+ LeaseService)",
        registry_addr
    );
    info!("  ✓ Proxy:          listening on {}", proxy_addr);
    info!(
        "  ✓ TransactionMgr: listening on {} (+ LeaseService)",
        txn_addr
    );
    info!(
        "  ✓ EventMgr:       listening on {} (+ LeaseService)",
        event_addr
    );
    info!(
        "  ✓ Space:          listening on {} (+ LeaseService)",
        space_addr
    );

    // Self-register every core service into the same process's Registry —
    // see spawn_bundled_self_register's doc comment for why this matters now.
    // No "LeaseMgr" entry — leasing is distributed, there's no single
    // interface to look up; a holder renews via the grantor_host/port
    // already carried on the Lease it holds.
    spawn_bundled_self_register("EventMgr", 9005, auth_config.clone());
    spawn_bundled_self_register("Proxy", 9003, auth_config.clone());
    spawn_bundled_self_register("TransactionMgr", 9004, auth_config.clone());
    spawn_bundled_self_register("Space", 9006, auth_config.clone());

    info!("Djinn ready.");

    tokio::try_join!(
        Server::builder()
            .layer(coordin8_observability::server_layer())
            .add_service(registry_svc)
            .add_service(lease_svc_for_registry)
            .serve(registry_addr),
        Server::builder()
            .layer(coordin8_observability::server_layer())
            .add_service(proxy_svc)
            .serve(proxy_addr),
        Server::builder()
            .layer(coordin8_observability::server_layer())
            .add_service(txn_svc)
            .add_service(lease_svc_for_txn)
            .serve(txn_addr),
        Server::builder()
            .layer(coordin8_observability::server_layer())
            .add_service(event_svc)
            .add_service(lease_svc_for_event)
            .serve(event_addr),
        Server::builder()
            .layer(coordin8_observability::server_layer())
            .add_service(space_svc)
            .add_service(space_participant_svc)
            .add_service(lease_svc_for_space)
            .serve(space_addr),
    )?;

    Ok(())
}

// ── Registry alone ────────────────────────────────────────────────────────────

/// Boot Registry alone on the given bind address.
///
/// Registry is the well-known anchor and does not self-register. It embeds
/// its own `LeaseManager` for its own entries' TTL bookkeeping — no external
/// dependency at all, so it's healthy and serving the moment it binds.
///
/// Pass `bind` as `"0.0.0.0:0"` to let the OS assign a port. Use
/// `serve_with_incoming` if you need the actual port before serving — see
/// [`run_registry_on_listener`] for the test-friendly version.
pub async fn run_registry() -> Result<()> {
    let bind = bind_addr();
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    let actual_addr = listener.local_addr()?;
    info!("Djinn registry starting on {actual_addr}...");
    run_registry_on_listener(listener).await
}

/// Boot Registry on a pre-bound [`TcpListener`].
///
/// This variant is test-friendly: bind port 0, read the actual address, then
/// pass the listener here. The caller knows the exact address before the
/// server starts accepting.
pub async fn run_registry_on_listener(listener: tokio::net::TcpListener) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let host = advertise_host();

    let auth_config = AuthConfig::from_env();

    let registry_store = registry_store_from_env().await?;
    let registry_index = Arc::new(RegistryIndex::new(registry_store));

    let (lease_manager, lease_svc) =
        embedded_landlord("registry", &host, actual_addr.port(), &auth_config).await?;
    let (registry_tx, _): (RegistryBroadcast, _) = broadcast::channel(256);

    {
        let registry_expiry_index = Arc::clone(&registry_index);
        let registry_expiry_tx = registry_tx.clone();
        spawn_cascade(
            "registry",
            lease_manager.expiry_tx().subscribe(),
            move |LeaseReclaimed { record, .. }| {
                let index = Arc::clone(&registry_expiry_index);
                let tx = registry_expiry_tx.clone();
                tokio::spawn(async move {
                    if let Ok(Some(entry)) = index.unregister_by_lease(&record.lease_id).await {
                        tracing::debug!(
                            capability_id = %entry.capability_id,
                            interface = %entry.interface,
                            lease_id = %record.lease_id,
                            "registry entry reclaimed (split mode)"
                        );
                        let _ = tx.send(coordin8_registry::service::RegistryChangedEvent {
                            event_type: 1,
                            entry,
                        });
                    }
                });
            },
        );
    }

    let leasing: Arc<dyn Leasing> = lease_manager;
    let registry_svc = RegistryServiceServer::with_interceptor(
        RegistryServiceImpl::new(
            registry_index,
            leasing,
            registry_tx,
            &host,
            actual_addr.port(),
        ),
        auth_config.clone(),
    );

    // No blocking external dependency (leasing is embedded, not remote) —
    // healthy the moment it's about to serve.
    let (mut health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_service_status("", ServingStatus::Serving)
        .await;

    info!("  ✓ Registry (split): listening on {actual_addr} (+ LeaseService)");

    Server::builder()
        .layer(coordin8_observability::server_layer())
        .add_service(health_service)
        .add_service(registry_svc)
        .add_service(lease_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
        .await?;

    Ok(())
}

// ── EventMgr alone ───────────────────────────────────────────────────────────

/// Boot EventMgr alone. Reads `COORDIN8_BIND_ADDR` and `COORDIN8_REGISTRY`.
///
/// Requires `COORDIN8_REGISTRY` to be set — EventMgr self-registers into it
/// (unrelated to leasing, which is now embedded).
pub async fn run_event() -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&bind_addr()).await?;
    let host = advertise_host();
    let registry_addr = std::env::var("COORDIN8_REGISTRY")
        .map_err(|_| anyhow::anyhow!("COORDIN8_REGISTRY must be set for split-mode EventMgr"))?;
    run_event_on_listener(listener, &registry_addr, &host, 30).await
}

/// Boot EventMgr on a pre-bound [`TcpListener`], with its own embedded
/// `LeaseManager` for subscription leases.
///
/// - `registry_addr` — used only for self-registration under
///   `interface=EventMgr`; EventMgr no longer has any leasing dependency on
///   Registry or anything else.
/// - `advertise_host` — host peers should use to dial this EventMgr.
/// - `self_lease_ttl` — TTL in seconds for the self-registration lease.
pub async fn run_event_on_listener(
    listener: tokio::net::TcpListener,
    registry_addr: &str,
    advertise_host: &str,
    self_lease_ttl: u64,
) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let advertise_port = actual_addr.port();
    let auth_config = AuthConfig::from_env();

    let (lease_manager, lease_svc) =
        embedded_landlord("event", advertise_host, advertise_port, &auth_config).await?;
    let leasing: Arc<dyn Leasing> = lease_manager.clone();

    let event_store = event_store_from_env().await?;
    let (event_tx, _) = broadcast::channel::<coordin8_core::EventRecord>(256);
    let event_manager = Arc::new(EventManager::new(event_store, leasing, event_tx));

    {
        let event_expiry_mgr = Arc::clone(&event_manager);
        spawn_cascade(
            "event",
            lease_manager.expiry_tx().subscribe(),
            move |LeaseReclaimed { record, .. }| {
                let mgr = Arc::clone(&event_expiry_mgr);
                tokio::spawn(async move {
                    let _ = mgr.unsubscribe_by_lease(&record.lease_id).await;
                });
            },
        );
    }

    let event_svc = EventServiceServer::with_interceptor(
        EventServiceImpl::new(Arc::clone(&event_manager), advertise_host, advertise_port),
        auth_config.clone(),
    );

    // No blocking external dependency — healthy the moment it's about to serve.
    let (mut health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_service_status("", ServingStatus::Serving)
        .await;

    info!(
        "  ✓ EventMgr (split): listening on {actual_addr} (+ LeaseService), advertising {advertise_host}:{advertise_port}"
    );

    let registry_url = registry_addr.to_string();
    let advertise_host_owned = advertise_host.to_string();
    let self_client_auth = auth_config.client_config("EventMgr");

    let register_fut = async move {
        let handle = self_register_retrying(
            &registry_url,
            &self_client_auth,
            "EventMgr",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await;
        info!(
            "  ✓ EventMgr: self-registered (capability: {}, lease: {})",
            handle.initial_capability_id(),
            handle.initial_lease_id()
        );
        std::future::pending::<()>().await;
    };

    let server_fut = Server::builder()
        .layer(coordin8_observability::server_layer())
        .add_service(health_service)
        .add_service(event_svc)
        .add_service(lease_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));

    info!("Djinn event ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}

// ── Space alone ──────────────────────────────────────────────────────────────

/// Boot Space alone. Reads `COORDIN8_BIND_ADDR` and `COORDIN8_REGISTRY`.
///
/// Requires `COORDIN8_REGISTRY` — Space self-registers into it and discovers
/// TxnMgr through it lazily for auto-enlist (unrelated to leasing, which is
/// now embedded).
pub async fn run_space() -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&bind_addr()).await?;
    let host = advertise_host();
    let registry_addr = std::env::var("COORDIN8_REGISTRY")
        .map_err(|_| anyhow::anyhow!("COORDIN8_REGISTRY must be set for split-mode Space"))?;
    run_space_on_listener(listener, &registry_addr, &host, 30).await
}

/// Boot Space on a pre-bound [`TcpListener`], with its own embedded
/// `LeaseManager` for tuple and watch leases.
///
/// Mounts `SpaceServiceImpl`, `SpaceParticipantService`, and `LeaseService`
/// all on the same server. Distinguishes tuple (`space:`) vs. watch
/// (`space-watch:`) expiry via the resource_id prefix `SpaceManager` already
/// uses internally — that convention is now purely Space's own private
/// concern (its `LeaseManager` never holds any other service's leases), not
/// a cross-service namespace scheme.
pub async fn run_space_on_listener(
    listener: tokio::net::TcpListener,
    registry_addr: &str,
    advertise_host: &str,
    self_lease_ttl: u64,
) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let advertise_port = actual_addr.port();
    let auth_config = AuthConfig::from_env();

    let (lease_manager, lease_svc) =
        embedded_landlord("space", advertise_host, advertise_port, &auth_config).await?;
    let leasing: Arc<dyn Leasing> = lease_manager.clone();

    // Split mode: auto-enlist dials TxnMgr over Registry lazily — no I/O
    // happens at boot, so Space can come up with no TxnMgr in sight and still
    // serve non-transactional traffic. The first transactional write/take
    // drives discovery on demand.
    let space_enlister = Arc::new(RemoteTxnEnlister::new(
        registry_addr,
        auth_config.client_config("space"),
    ));
    let space_participant_endpoint = format!("{advertise_host}:{advertise_port}");

    let space_store = space_store_from_env().await?;
    let (space_tuple_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    let (space_expiry_tx, _) = broadcast::channel::<coordin8_core::TupleRecord>(256);
    let space_manager = Arc::new(SpaceManager::with_enlister(
        space_store,
        leasing,
        space_tuple_tx,
        space_expiry_tx,
        space_enlister,
        space_participant_endpoint,
    ));

    {
        let space_expiry_mgr = Arc::clone(&space_manager);
        spawn_cascade(
            "space",
            lease_manager.expiry_tx().subscribe(),
            move |LeaseReclaimed { record, .. }| {
                let mgr = Arc::clone(&space_expiry_mgr);
                tokio::spawn(async move {
                    if record.resource_id.starts_with("space:") {
                        mgr.on_tuple_expired(&record.lease_id).await;
                    } else if record.resource_id.starts_with("space-watch:") {
                        mgr.on_watch_expired(&record.lease_id).await;
                    }
                });
            },
        );
    }

    let space_svc = SpaceServiceServer::with_interceptor(
        SpaceServiceImpl::new(Arc::clone(&space_manager), advertise_host, advertise_port),
        auth_config.clone(),
    );
    let space_participant_svc = ParticipantServiceServer::with_interceptor(
        SpaceParticipantService::new(space_manager),
        auth_config.clone(),
    );

    // No blocking external dependency — healthy the moment it's about to serve.
    let (mut health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_service_status("", ServingStatus::Serving)
        .await;

    info!(
        "  ✓ Space (split): listening on {actual_addr} (+ LeaseService), advertising {advertise_host}:{advertise_port}"
    );

    let registry_url = registry_addr.to_string();
    let advertise_host_owned = advertise_host.to_string();
    let self_client_auth = auth_config.client_config("Space");

    let register_fut = async move {
        let handle = self_register_retrying(
            &registry_url,
            &self_client_auth,
            "Space",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await;
        info!(
            "  ✓ Space: self-registered (capability: {}, lease: {})",
            handle.initial_capability_id(),
            handle.initial_lease_id()
        );
        std::future::pending::<()>().await;
    };

    let server_fut = Server::builder()
        .layer(coordin8_observability::server_layer())
        .add_service(health_service)
        .add_service(space_svc)
        .add_service(space_participant_svc)
        .add_service(lease_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));

    info!("Djinn space ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}

// ── TransactionMgr alone ─────────────────────────────────────────────────────

/// Boot TransactionMgr alone. Reads `COORDIN8_BIND_ADDR` and `COORDIN8_REGISTRY`.
///
/// Requires `COORDIN8_REGISTRY` — TxnMgr self-registers into it (unrelated to
/// leasing, which is now embedded).
pub async fn run_txn() -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&bind_addr()).await?;
    let host = advertise_host();
    let registry_addr = std::env::var("COORDIN8_REGISTRY").map_err(|_| {
        anyhow::anyhow!("COORDIN8_REGISTRY must be set for split-mode TransactionMgr")
    })?;
    run_txn_on_listener(listener, &registry_addr, &host, 30).await
}

/// Boot TransactionMgr on a pre-bound [`TcpListener`], with its own embedded
/// `LeaseManager` for transaction leases.
///
/// The txn id is recovered from the `txn:` prefix `TxnManager` already uses
/// internally when granting — that convention is now purely TxnMgr's own
/// private concern, not a cross-service namespace scheme.
pub async fn run_txn_on_listener(
    listener: tokio::net::TcpListener,
    registry_addr: &str,
    advertise_host: &str,
    self_lease_ttl: u64,
) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let advertise_port = actual_addr.port();
    let auth_config = AuthConfig::from_env();

    let (lease_manager, lease_svc) =
        embedded_landlord("txn", advertise_host, advertise_port, &auth_config).await?;
    let leasing: Arc<dyn Leasing> = lease_manager.clone();

    let txn_store = txn_store_from_env().await?;
    let txn_manager = Arc::new(TxnManager::with_client_auth(
        txn_store,
        leasing,
        auth_config.client_config("txn"),
    ));

    {
        let expiry_txn_mgr = Arc::clone(&txn_manager);
        spawn_cascade(
            "txn",
            lease_manager.expiry_tx().subscribe(),
            move |LeaseReclaimed { record, .. }| {
                if let Some(txn_id) = record.resource_id.strip_prefix("txn:") {
                    let mgr = Arc::clone(&expiry_txn_mgr);
                    let txn_id = txn_id.to_string();
                    tokio::spawn(async move {
                        let _ = mgr.abort_expired(&txn_id).await;
                    });
                }
            },
        );
    }

    let txn_svc = TransactionServiceServer::with_interceptor(
        TxnServiceImpl::new(txn_manager, advertise_host, advertise_port),
        auth_config.clone(),
    );

    // No blocking external dependency — healthy the moment it's about to serve.
    let (mut health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_service_status("", ServingStatus::Serving)
        .await;

    info!(
        "  ✓ TransactionMgr (split): listening on {actual_addr} (+ LeaseService), advertising {advertise_host}:{advertise_port}"
    );

    let registry_url = registry_addr.to_string();
    let advertise_host_owned = advertise_host.to_string();
    let self_client_auth = auth_config.client_config("TransactionMgr");

    let register_fut = async move {
        let handle = self_register_retrying(
            &registry_url,
            &self_client_auth,
            "TransactionMgr",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await;
        info!(
            "  ✓ TransactionMgr: self-registered (capability: {}, lease: {})",
            handle.initial_capability_id(),
            handle.initial_lease_id()
        );
        std::future::pending::<()>().await;
    };

    let server_fut = Server::builder()
        .layer(coordin8_observability::server_layer())
        .add_service(health_service)
        .add_service(txn_svc)
        .add_service(lease_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));

    info!("Djinn txn ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}

// ── Proxy alone ──────────────────────────────────────────────────────────────

/// Boot Proxy alone. Reads `COORDIN8_BIND_ADDR` and `COORDIN8_REGISTRY`.
///
/// Requires `COORDIN8_REGISTRY` — Proxy resolves capability templates by
/// calling the Registry's `Lookup` RPC via `RemoteCapabilityResolver`. Proxy
/// never grants leases, so it has no leasing wiring of its own — this
/// dependency on Registry is unrelated to (and unaffected by) distributed
/// leasing.
pub async fn run_proxy() -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&bind_addr()).await?;
    let host = advertise_host();
    let registry_addr = std::env::var("COORDIN8_REGISTRY")
        .map_err(|_| anyhow::anyhow!("COORDIN8_REGISTRY must be set for split-mode Proxy"))?;
    run_proxy_on_listener(listener, &registry_addr, &host, 30).await
}

/// Boot Proxy on a pre-bound [`TcpListener`] against a remote Registry.
///
/// Resolution is done through `RemoteCapabilityResolver`, so every open and
/// every forwarded connection issues a Registry `Lookup` RPC — the same
/// "resolve at connection time" semantics as the monolith, but over the
/// wire. Reads `PROXY_BIND_HOST` / `PROXY_PORT_MIN` / `PROXY_PORT_MAX` via
/// `ProxyConfig::from_env`.
pub async fn run_proxy_on_listener(
    listener: tokio::net::TcpListener,
    registry_addr: &str,
    advertise_host: &str,
    self_lease_ttl: u64,
) -> Result<()> {
    let actual_addr = listener.local_addr()?;
    let advertise_port = actual_addr.port();
    let auth_config = AuthConfig::from_env();

    // Construct immediately against a not-yet-resolved Registry — see
    // PendingCapabilityResolver. Discovery happens in the background task
    // below; requests made before it resolves fail fast with Unavailable
    // instead of the server not being reachable at all.
    let pending_resolver = Arc::new(coordin8_bootstrap::PendingCapabilityResolver::new());
    let resolver: Arc<dyn coordin8_core::CapabilityResolver> =
        Arc::clone(&pending_resolver) as Arc<dyn coordin8_core::CapabilityResolver>;
    let proxy_config = ProxyConfig::from_env();
    let proxy_manager = Arc::new(ProxyManager::new(resolver, proxy_config));

    let (mut health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_service_status("", ServingStatus::NotServing)
        .await;

    // Resolve Registry in the background; flip health to Serving once found.
    {
        let pending_resolver = Arc::clone(&pending_resolver);
        let registry_addr = registry_addr.to_string();
        let mut health_reporter = health_reporter.clone();
        let client_auth = auth_config.client_config("proxy");
        tokio::spawn(async move {
            let resolved = RemoteCapabilityResolver::connect(&registry_addr, client_auth)
                .await
                .expect("RemoteCapabilityResolver::connect retries forever, never returns Err");
            pending_resolver.install(resolved).await;
            health_reporter
                .set_service_status("", ServingStatus::Serving)
                .await;
            info!("  ✓ Proxy: Registry resolved, now Serving");
        });
    }

    let proxy_svc = ProxyServiceServer::with_interceptor(
        ProxyServiceImpl::new(proxy_manager),
        auth_config.clone(),
    );

    info!(
        "  ✓ Proxy (split): listening on {actual_addr}, advertising {advertise_host}:{advertise_port}"
    );

    let registry_url = registry_addr.to_string();
    let advertise_host_owned = advertise_host.to_string();
    let self_client_auth = auth_config.client_config("Proxy");

    let register_fut = async move {
        let handle = self_register_retrying(
            &registry_url,
            &self_client_auth,
            "Proxy",
            std::collections::HashMap::new(),
            &advertise_host_owned,
            advertise_port,
            self_lease_ttl,
        )
        .await;
        info!(
            "  ✓ Proxy: self-registered (capability: {}, lease: {})",
            handle.initial_capability_id(),
            handle.initial_lease_id()
        );
        std::future::pending::<()>().await;
    };

    let server_fut = Server::builder()
        .layer(coordin8_observability::server_layer())
        .add_service(health_service)
        .add_service(proxy_svc)
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));

    info!("Djinn proxy ready.");

    tokio::select! {
        res = server_fut => res?,
        _ = register_fut => unreachable!("register_fut awaits pending() forever"),
    }

    Ok(())
}

// ── Healthcheck (CLI) ─────────────────────────────────────────────────────────

/// Check a Djinn service's health via the standard gRPC Health Checking
/// Protocol. Connects to `addr`, calls `Check` for the overall ("") service
/// name, and returns `Ok(())` only if the reported status is `Serving`.
///
/// Bounded by an overall 3s timeout so a hung dial can't wedge `docker
/// healthcheck` past its own `timeout:` setting.
pub async fn run_healthcheck(addr: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let channel = tonic::transport::Channel::from_shared(addr.to_string())
            .map_err(|e| anyhow::anyhow!("invalid addr: {e}"))?
            .connect()
            .await
            .map_err(|e| anyhow::anyhow!("connect failed: {e}"))?;
        let mut client = tonic_health::pb::health_client::HealthClient::new(channel);

        let resp = client
            .check(tonic_health::pb::HealthCheckRequest {
                service: String::new(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("check rpc failed: {e}"))?
            .into_inner();

        if resp.status() == tonic_health::pb::health_check_response::ServingStatus::Serving {
            Ok(())
        } else {
            anyhow::bail!("status: {:?}", resp.status())
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("healthcheck timed out"))?
}
