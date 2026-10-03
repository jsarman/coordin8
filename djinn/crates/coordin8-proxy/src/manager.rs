use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};
use uuid::Uuid;

use coordin8_core::{CapabilityResolver, LeaseReclaimed, LeaseRecord, Leasing};

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
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            bind_host: "127.0.0.1".to_string(),
            port_range: None,
        }
    }
}

impl ProxyConfig {
    /// Load from environment variables:
    ///   PROXY_BIND_HOST  (default: 127.0.0.1)
    ///   PROXY_PORT_MIN   (optional)
    ///   PROXY_PORT_MAX   (optional)
    pub fn from_env() -> Self {
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
        Self {
            bind_host,
            port_range,
        }
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
        let _ = self.resolve(&template).await?;

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
        let pid = proxy_id.clone();

        info!(proxy_id = %pid, local_port, "proxy opened");

        tokio::spawn(async move {
            tokio::select! {
                _ = accept_loop(listener, resolver, template) => {}
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
        if let Some((_, entry)) = self.proxies.remove(proxy_id) {
            let _ = entry.shutdown.send(());
            info!(proxy_id, reason = ?event.reason, "proxy lease reclaimed, listener closed");
        }
    }

    async fn resolve(&self, template: &HashMap<String, String>) -> Result<String, ProxyError> {
        let entry = self
            .resolver
            .resolve(template)
            .await?
            .ok_or_else(|| ProxyError::NotFound(template.clone()))?;

        let t = entry
            .transport
            .ok_or_else(|| ProxyError::NotFound(template.clone()))?;
        let host = t.config.get("host").cloned().unwrap_or_default();
        let port = t.config.get("port").cloned().unwrap_or_default();
        Ok(format!("{host}:{port}"))
    }
}

async fn accept_loop(
    listener: TcpListener,
    resolver: Arc<dyn CapabilityResolver>,
    template: HashMap<String, String>,
) {
    loop {
        match listener.accept().await {
            Ok((client, peer)) => {
                debug!(%peer, "proxy accepted connection");
                let r = resolver.clone();
                let tmpl = template.clone();
                tokio::spawn(async move {
                    if let Err(e) = forward(client, r, tmpl).await {
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
    template: HashMap<String, String>,
) -> std::io::Result<()> {
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

    let t = match entry.transport {
        Some(t) => t,
        None => return Ok(()),
    };

    let host = t.config.get("host").cloned().unwrap_or_default();
    let port = t.config.get("port").cloned().unwrap_or_default();
    let addr = format!("{host}:{port}");

    let mut upstream = TcpStream::connect(&addr).await?;
    debug!(%addr, "proxy connected to upstream");

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
