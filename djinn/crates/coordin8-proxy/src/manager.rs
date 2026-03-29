use std::collections::HashMap;
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
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store error: {0}")]
    Store(#[from] coordin8_core::error::Error),
}

struct ProxyEntry {
    local_port: u16,
    shutdown: oneshot::Sender<()>,
}

pub struct ProxyManager {
    registry: Arc<dyn RegistryStore>,
    proxies: Arc<DashMap<String, ProxyEntry>>,
}

impl ProxyManager {
    pub fn new(registry: Arc<dyn RegistryStore>) -> Self {
        Self {
            registry,
            proxies: Arc::new(DashMap::new()),
        }
    }

    pub async fn open(
        &self,
        template: HashMap<String, String>,
    ) -> Result<(String, u16), ProxyError> {
        // Fail fast if nothing matches right now
        let _ = self.resolve(&template).await?;

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_port = listener.local_addr()?.port();
        let proxy_id = Uuid::new_v4().to_string();

        let (tx, rx) = oneshot::channel::<()>();

        let proxies = self.proxies.clone();
        let registry = self.registry.clone();
        let pid = proxy_id.clone();

        info!(
            proxy_id = %pid,
            local_port,
            "proxy opened"
        );

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

    async fn resolve(
        &self,
        template: &HashMap<String, String>,
    ) -> Result<String, ProxyError> {
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
    let entries = registry
        .list_all()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|e| {
            let mut all_attrs = e.attrs.clone();
            all_attrs.insert("interface".to_string(), e.interface.clone());
            matches(&ops, &all_attrs)
        })
        .collect::<Vec<_>>();

    let entry = match entries.into_iter().next() {
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
