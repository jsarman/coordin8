use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};
use tracing::debug;
use uuid::Uuid;

use coordin8_core::{RegistryEntry, TransportConfig};
use coordin8_proto::coordin8::{
    registry_service_server::RegistryService, Capability, LookupRequest, RegisterRequest,
    RegistryEvent, RegistryWatchRequest,
};
use coordin8_proto::coordin8::{Lease, TransportDescriptor};

use crate::store::RegistryIndex;

/// Broadcast channel payload for registry change events.
#[derive(Debug, Clone)]
pub struct RegistryChangedEvent {
    pub event_type: i32, // RegistryEvent::EventType
    pub entry: RegistryEntry,
}

pub type RegistryBroadcast = broadcast::Sender<RegistryChangedEvent>;

fn entry_to_capability(e: RegistryEntry) -> Capability {
    Capability {
        capability_id: e.capability_id,
        interface: e.interface,
        attrs: e.attrs,
        transport: e.transport.map(|t| TransportDescriptor {
            r#type: t.transport_type,
            config: t.config,
        }),
    }
}

pub struct RegistryServiceImpl {
    index: Arc<RegistryIndex>,
    lease_manager: Arc<coordin8_lease::LeaseManager>,
    event_tx: RegistryBroadcast,
}

impl RegistryServiceImpl {
    pub fn new(
        index: Arc<RegistryIndex>,
        lease_manager: Arc<coordin8_lease::LeaseManager>,
        event_tx: RegistryBroadcast,
    ) -> Self {
        Self {
            index,
            lease_manager,
            event_tx,
        }
    }
}

type BoxStream<T> = Pin<Box<dyn futures_core::Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl RegistryService for RegistryServiceImpl {
    async fn register(&self, req: Request<RegisterRequest>) -> Result<Response<Lease>, Status> {
        let r = req.into_inner();
        let capability_id = Uuid::new_v4().to_string();
        let resource_id = format!("registry:{}", capability_id);

        let lease = self
            .lease_manager
            .grant(&resource_id, r.ttl_seconds)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let transport_type = r
            .transport
            .as_ref()
            .map(|t| t.r#type.clone())
            .unwrap_or_else(|| "none".to_string());

        let entry = RegistryEntry {
            capability_id: capability_id.clone(),
            lease_id: lease.lease_id.clone(),
            interface: r.interface.clone(),
            attrs: r.attrs,
            transport: r.transport.map(|t| TransportConfig {
                transport_type: t.r#type,
                config: t.config,
            }),
        };

        self.index
            .register(entry.clone())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        debug!(
            capability_id,
            interface = %entry.interface,
            lease_id = %lease.lease_id,
            ttl_secs = r.ttl_seconds,
            transport = transport_type,
            "service registered"
        );

        let _ = self.event_tx.send(RegistryChangedEvent {
            event_type: 0, // REGISTERED
            entry,
        });

        use prost_types::Timestamp;
        Ok(Response::new(Lease {
            lease_id: lease.lease_id,
            resource_id: lease.resource_id,
            granted_at: Some(Timestamp {
                seconds: lease.granted_at.timestamp(),
                nanos: lease.granted_at.timestamp_subsec_nanos() as i32,
            }),
            expires_at: Some(Timestamp {
                seconds: lease.expires_at.timestamp(),
                nanos: lease.expires_at.timestamp_subsec_nanos() as i32,
            }),
        }))
    }

    async fn lookup(&self, req: Request<LookupRequest>) -> Result<Response<Capability>, Status> {
        let template = req.into_inner().template;
        let result = self
            .index
            .lookup(&template)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        match result {
            Some(entry) => {
                debug!(
                    capability_id = %entry.capability_id,
                    interface = %entry.interface,
                    ?template,
                    "lookup hit"
                );
                Ok(Response::new(entry_to_capability(entry)))
            }
            None => {
                debug!(?template, "lookup miss — no matching capability");
                Err(Status::not_found("no matching capability"))
            }
        }
    }

    type LookupAllStream = BoxStream<Capability>;

    async fn lookup_all(
        &self,
        req: Request<LookupRequest>,
    ) -> Result<Response<Self::LookupAllStream>, Status> {
        let template = req.into_inner().template;
        let entries = self
            .index
            .lookup_all(&template)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        debug!(?template, count = entries.len(), "lookup_all");

        let stream = tokio_stream::iter(entries.into_iter().map(|e| Ok(entry_to_capability(e))));
        Ok(Response::new(Box::pin(stream)))
    }

    type WatchStream = BoxStream<RegistryEvent>;

    async fn watch(
        &self,
        req: Request<RegistryWatchRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let template = req.into_inner().template;
        debug!(?template, "watch subscribed");
        let rx = self.event_tx.subscribe();

        let stream = BroadcastStream::new(rx).filter_map(move |result| {
            let tmpl = template.clone();
            match result {
                Ok(evt) => {
                    let mut combined = evt.entry.attrs.clone();
                    combined.insert("interface".to_string(), evt.entry.interface.clone());
                    let ops = crate::matcher::parse_template(&tmpl);
                    if crate::matcher::matches(&ops, &combined) {
                        Some(Ok(RegistryEvent {
                            r#type: evt.event_type,
                            capability: Some(entry_to_capability(evt.entry)),
                        }))
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        });

        Ok(Response::new(Box::pin(stream)))
    }
}
