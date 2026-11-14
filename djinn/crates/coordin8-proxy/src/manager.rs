use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};
use uuid::Uuid;

use coordin8_core::{
    is_metadata_range, CapabilityResolver, EndpointAllowlist, LeaseReclaimed, LeaseRecord, Leasing,
    RegistryEntry, PROXY_UPSTREAM_ALLOW_VAR,
};

/// How long a forwarded connection waits for the upstream to accept.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Resource-id prefix for proxy leases: `proxy:<proxy_id>`.
pub const PROXY_RESOURCE_PREFIX: &str = "proxy:";

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("no capability found for template: {0:?}")]
    NotFound(HashMap<String, String>),
    #[error("proxy not found: {0}")]
    ProxyNotFound(String),
    #[error("proxy port range exhausted")]
    PortRangeExhausted,
    #[error("{0}")]
    Forbidden(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store error: {0}")]
    Store(#[from] coordin8_core::error::Error),
}

struct ProxyEntry {
    lease_id: String,
    shutdown: oneshot::Sender<()>,
}

/// Configuration for the proxy port binding.
#[derive(Clone)]
pub struct ProxyConfig {
    /// Host to bind proxy listeners on. Use `0.0.0.0` when running in Docker.
    pub bind_host: String,
    /// Optional fixed port range for proxy listeners.
    /// When set, ports are allocated sequentially within [min, max].
    /// When None, the OS picks an ephemeral port.
    pub port_range: Option<(u16, u16)>,
    /// Upstreams the proxy may forward to (`COORDIN8_PROXY_UPSTREAM_ALLOW`).
    /// Link-local addresses are refused unless an IP/CIDR entry names them.
    pub upstream_allow: EndpointAllowlist,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            bind_host: "127.0.0.1".to_string(),
            port_range: None,
            upstream_allow: EndpointAllowlist::allow_all(PROXY_UPSTREAM_ALLOW_VAR),
        }
    }
}

impl ProxyConfig {
    /// Load from environment variables:
    ///   PROXY_BIND_HOST  (default: 127.0.0.1)
    ///   PROXY_PORT_MIN   (optional)
    ///   PROXY_PORT_MAX   (optional)
    ///   COORDIN8_PROXY_UPSTREAM_ALLOW (optional; invalid entries are an error)
    pub fn from_env() -> Result<Self, String> {
        let bind_host =
            std::env::var("PROXY_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port_range = match (
            std::env::var("PROXY_PORT_MIN")
                .ok()
                .and_then(|v| v.parse::<u16>().ok()),
            std::env::var("PROXY_PORT_MAX")
                .ok()
                .and_then(|v| v.parse::<u16>().ok()),
        ) {
            (Some(min), Some(max)) => Some((min, max)),
            _ => None,
        };
        Ok(Self {
            bind_host,
            port_range,
            upstream_allow: EndpointAllowlist::from_env(PROXY_UPSTREAM_ALLOW_VAR)?,
        })
    }
}

pub struct ProxyManager {
    resolver: Arc<dyn CapabilityResolver>,
    leasing: Arc<dyn Leasing>,
    proxies: Arc<DashMap<String, ProxyEntry>>,
    config: ProxyConfig,
    next_port: Arc<std::sync::atomic::AtomicU16>,
}

impl ProxyManager {
    /// `leasing` is the Proxy's own embedded lease grantor (namespace
    /// `"proxy"`). Every open proxy holds a lease; when it is reclaimed
    /// (expiry or cancel) the owner of this manager must route the event to
    /// [`ProxyManager::handle_reclaimed`].
    pub fn new(
        resolver: Arc<dyn CapabilityResolver>,
        config: ProxyConfig,
        leasing: Arc<dyn Leasing>,
    ) -> Self {
        let next_port = config.port_range.map(|(min, _)| min).unwrap_or(0);
        Self {
            resolver,
            leasing,
            proxies: Arc::new(DashMap::new()),
            config,
            next_port: Arc::new(std::sync::atomic::AtomicU16::new(next_port)),
        }
    }

    async fn bind_listener(&self) -> Result<TcpListener, ProxyError> {
        match self.config.port_range {
            Some((min, max)) => {
                for _ in min..=max {
                    let port = self
                        .next_port
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let port = if port > max { min } else { port };
                    self.next_port.store(
                        if port >= max { min } else { port + 1 },
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    let addr = format!("{}:{}", self.config.bind_host, port);
                    match TcpListener::bind(&addr).await {
                        Ok(l) => return Ok(l),
                        Err(_) => continue,
                    }
                }
                Err(ProxyError::PortRangeExhausted)
            }
            None => {
                let addr = format!("{}:0", self.config.bind_host);
                Ok(TcpListener::bind(&addr).await?)
            }
        }
    }

    pub async fn open(
        &self,
        template: HashMap<String, String>,
        ttl_secs: u64,
    ) -> Result<(String, u16, LeaseRecord), ProxyError> {
        // Fail early: an upstream the proxy would refuse is refused here.
        let entry = self
            .resolver
            .resolve(&template)
            .await?
            .ok_or_else(|| ProxyError::NotFound(template.clone()))?;
        vet_upstream(&self.config.upstream_allow, &entry, &template).await?;

        let listener = self.bind_listener().await?;
        let local_port = listener.local_addr()?.port();
        let proxy_id = Uuid::new_v4().to_string();
        // Grant before spawning: if this fails the listener is just dropped.
        let lease = self
            .leasing
            .grant(&format!("{PROXY_RESOURCE_PREFIX}{proxy_id}"), ttl_secs)
            .await?;

        let (tx, rx) = oneshot::channel::<()>();

        let proxies = self.proxies.clone();
        let resolver = self.resolver.clone();
        let allow = self.config.upstream_allow.clone();
        let pid = proxy_id.clone();

        info!(proxy_id = %pid, local_port, "proxy opened");

        tokio::spawn(async move {
            tokio::select! {
                _ = accept_loop(listener, resolver, allow, template) => {}
                _ = rx => {
                    debug!(proxy_id = %pid, "proxy shut down");
                }
            }
            proxies.remove(&pid);
        });

        self.proxies.insert(
            proxy_id.clone(),
            ProxyEntry {
                lease_id: lease.lease_id.clone(),
                shutdown: tx,
            },
        );

        Ok((proxy_id, local_port, lease))
    }

    pub async fn close(&self, proxy_id: &str) -> Result<(), ProxyError> {
        let (_, entry) = self
            .proxies
            .remove(proxy_id)
            .ok_or_else(|| ProxyError::ProxyNotFound(proxy_id.to_string()))?;
        let _ = entry.shutdown.send(());
        // Entry is already gone, so the cascade triggered by this cancel is a no-op.
        if let Err(e) = self.leasing.cancel(&entry.lease_id).await {
            warn!(proxy_id, "proxy lease cancel failed: {e}");
        }
        debug!(proxy_id, "proxy closed");
        Ok(())
    }

    /// Cascade target: a lease was reclaimed (expired or cancelled). If it
    /// belongs to a proxy, close that proxy's listener exactly like `Release`.
    /// Idempotent — unknown or already-closed proxies are ignored.
    pub fn handle_reclaimed(&self, event: &LeaseReclaimed) {
        let Some(proxy_id) = event.record.resource_id.strip_prefix(PROXY_RESOURCE_PREFIX) else {
            return;
        };
        // Only the proxy's own lease closes it: `Grant` accepts any resource
        // id, so a lease named `proxy:<victim>` must not close the victim.
        if let Some((_, entry)) = self
            .proxies
            .remove_if(proxy_id, |_, e| e.lease_id == event.record.lease_id)
        {
            let _ = entry.shutdown.send(());
            info!(proxy_id, reason = ?event.reason, "proxy lease reclaimed, listener closed");
        }
    }
}

/// Resolve an entry's transport to the addresses the proxy may dial. Each
/// resolved address must be allowed (literal `host:port` match, or an IP/CIDR
/// entry covering it), and a link-local address (cloud metadata) only when an
/// IP/CIDR entry names it. The proxy then dials exactly the vetted address, so
/// a DNS answer that changes between check and connect can't slip past.
async fn vet_upstream(
    allow: &EndpointAllowlist,
    entry: &RegistryEntry,
    template: &HashMap<String, String>,
) -> Result<Vec<SocketAddr>, ProxyError> {
    let t = entry
        .transport
        .as_ref()
        .ok_or_else(|| ProxyError::NotFound(template.clone()))?;
    let host = t.config.get("host").map(String::as_str).unwrap_or_default();
    let port = t.config.get("port").map(String::as_str).unwrap_or_default();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let endpoint = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let literal_ok = match allow.check(&endpoint) {
        Ok(()) => true,
        Err(coordin8_core::Error::InvalidArgument(e)) => {
            return Err(ProxyError::Forbidden(format!("bad upstream: {e}")))
        }
        Err(_) => false,
    };
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host(&endpoint).await?.collect();
    let ok: Vec<SocketAddr> = addrs
        .iter()
        .copied()
        .filter(|a| {
            (literal_ok || allow.allows_ip(a.ip(), a.port()))
                && (!is_metadata_range(a.ip()) || allow.explicitly_allows_ip(a.ip(), a.port()))
        })
        .collect();
    if ok.is_empty() {
        return Err(ProxyError::Forbidden(format!(
            "upstream '{endpoint}' is not permitted by {} (link-local addresses need an explicit IP/CIDR entry)",
            allow.var()
        )));
    }
    Ok(ok)
}

async fn accept_loop(
    listener: TcpListener,
    resolver: Arc<dyn CapabilityResolver>,
    allow: EndpointAllowlist,
    template: HashMap<String, String>,
) {
    loop {
        match listener.accept().await {
            Ok((client, peer)) => {
                debug!(%peer, "proxy accepted connection");
                let r = resolver.clone();
                let a = allow.clone();
                let tmpl = template.clone();
                tokio::spawn(async move {
                    if let Err(e) = forward(client, r, &a, tmpl).await {
                        warn!("proxy forward error: {e}");
                    }
                });
            }
            Err(e) => {
                warn!("proxy accept error: {e}");
                break;
            }
        }
    }
}

async fn forward(
    mut client: TcpStream,
    resolver: Arc<dyn CapabilityResolver>,
    allow: &EndpointAllowlist,
    template: HashMap<String, String>,
) -> Result<(), ProxyError> {
    let entry = match resolver.resolve(&template).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            warn!(?template, "proxy: no capability at forward time");
            return Ok(());
        }
        Err(e) => {
            warn!(?template, "proxy: resolver error at forward time: {e}");
            return Ok(());
        }
    };
    // Re-vetted per connection: the entry (and its address) can change.
    let addrs = vet_upstream(allow, &entry, &template).await?;
    let mut upstream = tokio::time::timeout(
        UPSTREAM_CONNECT_TIMEOUT,
        TcpStream::connect(addrs.as_slice()),
    )
    .await
    .map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "upstream connect timed out")
    })??;
    debug!(?addrs, "proxy connected to upstream");

    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coordin8_core::{Error, LeaseConfig, ReclaimReason, RegistryEntry, TransportConfig};
    use coordin8_lease::LeaseManager;
    use coordin8_provider_local::InMemoryLeaseStore;
    use std::time::Duration;
    use tokio::sync::broadcast;

    struct Fixed;

    #[async_trait::async_trait]
    impl CapabilityResolver for Fixed {
        async fn resolve(
            &self,
            _t: &HashMap<String, String>,
        ) -> Result<Option<RegistryEntry>, Error> {
            let mut config = HashMap::new();
            config.insert("host".to_string(), "127.0.0.1".to_string());
            config.insert("port".to_string(), "1".to_string());
            Ok(Some(RegistryEntry {
                capability_id: "c".into(),
                lease_id: "l".into(),
                interface: "Svc".into(),
                attrs: HashMap::new(),
                transport: Some(TransportConfig {
                    transport_type: "tcp".into(),
                    config,
                }),
            }))
        }
    }

    /// Manager + embedded landlord + reaper + cascade, wired like services.rs.
    fn rig() -> Arc<ProxyManager> {
        let (tx, _) = broadcast::channel::<LeaseReclaimed>(64);
        let lm = Arc::new(LeaseManager::new(
            Arc::new(InMemoryLeaseStore::new()),
            LeaseConfig {
                max_ttl: None,
                preferred_ttl: 30,
            },
            tx.clone(),
        ));
        let (reaper_lm, reaper_tx) = (Arc::clone(&lm), tx.clone());
        tokio::spawn(async move {
            coordin8_lease::reaper::run_reaper(reaper_lm, reaper_tx, Duration::from_millis(100))
                .await;
        });
        let mgr = Arc::new(ProxyManager::new(
            Arc::new(Fixed),
            ProxyConfig::default(),
            lm,
        ));
        let (m, mut rx) = (Arc::clone(&mgr), tx.subscribe());
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                m.handle_reclaimed(&ev);
            }
        });
        mgr
    }

    async fn is_listening(port: u16) -> bool {
        TcpStream::connect(("127.0.0.1", port)).await.is_ok()
    }

    #[tokio::test]
    async fn abandoned_proxy_is_reclaimed_and_port_reusable() {
        let mgr = rig();
        let (id, port, lease) = mgr.open(HashMap::new(), 1).await.unwrap();
        assert_eq!(lease.resource_id, format!("proxy:{id}"));
        assert!(is_listening(port).await);

        // Never renewed: after the TTL + reaper tick the listener must be gone.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!is_listening(port).await, "abandoned proxy still listening");
        assert!(mgr.proxies.get(&id).is_none());
        // Port can be rebound.
        TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    }

    #[tokio::test]
    async fn renewed_proxy_stays_open() {
        let mgr = rig();
        let (_, port, lease) = mgr.open(HashMap::new(), 1).await.unwrap();
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            mgr.leasing.renew(&lease.lease_id, 1).await.unwrap();
        }
        assert!(is_listening(port).await, "renewed proxy was reclaimed");
    }

    #[tokio::test]
    async fn release_cancels_lease() {
        let mgr = rig();
        let (id, port, lease) = mgr.open(HashMap::new(), 60).await.unwrap();
        mgr.close(&id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!is_listening(port).await);
        assert!(
            mgr.leasing.renew(&lease.lease_id, 60).await.is_err(),
            "lease should be gone after Release"
        );
    }

    #[tokio::test]
    async fn forged_lease_on_proxy_resource_is_ignored() {
        let mgr = rig();
        let (id, port, _) = mgr.open(HashMap::new(), 60).await.unwrap();
        // Anyone can Grant a lease naming this proxy's resource id, then cancel it.
        let forged = mgr
            .leasing
            .grant(&format!("{PROXY_RESOURCE_PREFIX}{id}"), 60)
            .await
            .unwrap();
        mgr.leasing.cancel(&forged.lease_id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(is_listening(port).await, "forged lease closed the proxy");
        assert!(mgr.proxies.get(&id).is_some());
    }

    #[tokio::test]
    async fn link_local_upstream_is_refused_unless_named() {
        struct MetaData;
        #[async_trait::async_trait]
        impl CapabilityResolver for MetaData {
            async fn resolve(
                &self,
                _t: &HashMap<String, String>,
            ) -> Result<Option<RegistryEntry>, Error> {
                let config = HashMap::from([
                    ("host".to_string(), "169.254.169.254".to_string()),
                    ("port".to_string(), "80".to_string()),
                ]);
                Ok(Some(RegistryEntry {
                    capability_id: "c".into(),
                    lease_id: "l".into(),
                    interface: "Svc".into(),
                    attrs: HashMap::new(),
                    transport: Some(TransportConfig {
                        transport_type: "tcp".into(),
                        config,
                    }),
                }))
            }
        }
        let (tx, _) = broadcast::channel::<LeaseReclaimed>(8);
        let lm = Arc::new(LeaseManager::new(
            Arc::new(InMemoryLeaseStore::new()),
            LeaseConfig::default(),
            tx,
        ));
        let mgr = ProxyManager::new(Arc::new(MetaData), ProxyConfig::default(), lm.clone());
        assert!(matches!(
            mgr.open(HashMap::new(), 60).await,
            Err(ProxyError::Forbidden(_))
        ));
        let named = ProxyConfig {
            upstream_allow: EndpointAllowlist::parse(
                PROXY_UPSTREAM_ALLOW_VAR,
                "169.254.169.254:80",
            )
            .unwrap(),
            ..ProxyConfig::default()
        };
        let mgr = ProxyManager::new(Arc::new(MetaData), named, lm);
        assert!(mgr.open(HashMap::new(), 60).await.is_ok());
    }

    #[tokio::test]
    async fn upstream_outside_allowlist_is_refused() {
        let config = ProxyConfig {
            upstream_allow: EndpointAllowlist::parse(PROXY_UPSTREAM_ALLOW_VAR, "10.0.0.0/8, svc")
                .unwrap(),
            ..ProxyConfig::default()
        };
        let (tx, _) = broadcast::channel::<LeaseReclaimed>(8);
        let lm = Arc::new(LeaseManager::new(
            Arc::new(InMemoryLeaseStore::new()),
            LeaseConfig::default(),
            tx,
        ));
        // `Fixed` resolves to 127.0.0.1:1.
        let mgr = ProxyManager::new(Arc::new(Fixed), config, lm);
        assert!(matches!(
            mgr.open(HashMap::new(), 60).await,
            Err(ProxyError::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn handle_reclaimed_ignores_foreign_resources() {
        let mgr = rig();
        let (_, port, mut lease) = mgr.open(HashMap::new(), 60).await.unwrap();
        lease.resource_id = "space:xyz".into();
        mgr.handle_reclaimed(&LeaseReclaimed {
            record: lease,
            reason: ReclaimReason::Expired,
        });
        assert!(is_listening(port).await);
    }
}
