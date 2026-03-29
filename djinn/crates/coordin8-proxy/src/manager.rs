use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};
use uuid::Uuid;

use coordin8_core::registry::RegistryStore;
use coordin8_registry::matcher::{matches, parse_template};

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
    local_port: u16,
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
        let bind_host = std::env::var("PROXY_BIND_HOST")
            .unwrap_or_else(|_| "127.0.0.1".to_string());
        let port_range = match (
            std::env::var("PROXY_PORT_MIN").ok().and_then(|v| v.parse::<u16>().ok()),
            std::env::var("PROXY_PORT_MAX").ok().and_then(|v| v.parse::<u16>().ok()),
        ) {
            (Some(min), Some(max)) => Some((min, max)),
            _ => None,
        };
        Self { bind_host, port_range }
    }
}

pub struct ProxyManager {
    registry: Arc<dyn RegistryStore>,
    proxies: Arc<DashMap<String, ProxyEntry>>,
    config: ProxyConfig,
    next_port: Arc<std::sync::atomic::AtomicU16>,
}

impl ProxyManager {
    pub fn new(registry: Arc<dyn RegistryStore>, config: ProxyConfig) -> Self {
        let next_port = config.port_range.map(|(min, _)| min).unwrap_or(0);
        Self {
            registry,
            proxies: Arc::new(DashMap::new()),
            config,
            next_port: Arc::new(std::sync::atomic::AtomicU16::new(next_port)),
        }
    }

    async fn bind_listener(&self) -> Result<TcpListener, ProxyError> {
        match self.config.port_range {
            Some((min, max)) => {
                for _ in min..=max {
                    let port = self.next_port.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    ) -> Result<(String, u16), ProxyError> {
        let _ = self.resolve(&template).await?;

        let listener = self.bind_listener().await?;
        let local_port = listener.local_addr()?.port();
        let proxy_id = Uuid::new_v4().to_string();

        let (tx, rx) = oneshot::channel::<()>();

        let proxies = self.proxies.clone();
        let registry = self.registry.clone();
        let pid = proxy_id.clone();

        info!(proxy_id = %pid, local_port, "proxy opened");

        tokio::spawn(async move {
            tokio::select! {
                _ = accept_loop(listener, registry, template) => {}
                _ = rx => {
                    debug!(proxy_id = %pid, "proxy shut down");
                }
            }
            proxies.remove(&pid);
        });

        self.proxies.insert(
            proxy_id.clone(),
            ProxyEntry { local_port, shutdown: tx },
        );

        Ok((proxy_id, local_port))
    }

    pub async fn close(&self, proxy_id: &str) -> Result<(), ProxyError> {
        let (_, entry) = self
            .proxies
            .remove(proxy_id)
            .ok_or_else(|| ProxyError::ProxyNotFound(proxy_id.to_string()))?;
        let _ = entry.shutdown.send(());
        debug!(proxy_id, "proxy closed");
        Ok(())
    }

    async fn resolve(&self, template: &HashMap<String, String>) -> Result<String, ProxyError> {
        let ops = parse_template(template);
        let entries = self.registry.list_all().await?;

        let entry = entries
            .into_iter()
            .find(|e| {
                let mut all_attrs = e.attrs.clone();
                all_attrs.insert("interface".to_string(), e.interface.clone());
                matches(&ops, &all_attrs)
            })
            .ok_or_else(|| ProxyError::NotFound(template.clone()))?;

        let t = entry.transport.ok_or_else(|| ProxyError::NotFound(template.clone()))?;
        let host = t.config.get("host").cloned().unwrap_or_default();
        let port = t.config.get("port").cloned().unwrap_or_default();
        Ok(format!("{host}:{port}"))
    }
}

async fn accept_loop(
    listener: TcpListener,
    registry: Arc<dyn RegistryStore>,
    template: HashMap<String, String>,
) {
    loop {
        match listener.accept().await {
            Ok((client, peer)) => {
                debug!(%peer, "proxy accepted connection");
                let reg = registry.clone();
                let tmpl = template.clone();
                tokio::spawn(async move {
                    if let Err(e) = forward(client, reg, tmpl).await {
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
    registry: Arc<dyn RegistryStore>,
    template: HashMap<String, String>,
) -> std::io::Result<()> {
    let ops = parse_template(&template);
    let entry = registry
        .list_all()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|e| {
            let mut all_attrs = e.attrs.clone();
            all_attrs.insert("interface".to_string(), e.interface.clone());
            matches(&ops, &all_attrs)
        });

    let entry = match entry {
        Some(e) => e,
        None => {
            warn!(?template, "proxy: no capability at forward time");
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
