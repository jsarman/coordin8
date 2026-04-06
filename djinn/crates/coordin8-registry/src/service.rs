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
    registry_service_server::RegistryService, Capability, LookupRequest, ModifyAttrsRequest,
    RegisterRequest, RegisterResponse, RegistryEvent, RegistryWatchRequest,
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

fn to_timestamp(dt: chrono::DateTime<chrono::Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    }
}

fn entry_to_capability(e: &RegistryEntry) -> Capability {
    Capability {
        capability_id: e.capability_id.clone(),
        interface: e.interface.clone(),
        attrs: e.attrs.clone(),
        transport: e.transport.as_ref().map(|t| TransportDescriptor {
            r#type: t.transport_type.clone(),
            config: t.config.clone(),
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
    async fn register(
        &self,
        req: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let r = req.into_inner();

        let is_reregister = !r.capability_id.is_empty();

        if is_reregister {
            // Re-registration: update existing entry in-place.
            let existing = self
                .index
                .get(&r.capability_id)
                .await
                .map_err(|e| Status::internal(e.to_string()))?
                .ok_or_else(|| {
                    Status::not_found(format!("capability not found: {}", r.capability_id))
                })?;

            // Renew the existing lease.
            let lease = self
                .lease_manager
                .renew(&existing.lease_id, r.ttl_seconds)
                .await
                .map_err(|e| Status::failed_precondition(e.to_string()))?;

            let entry = RegistryEntry {
                capability_id: r.capability_id.clone(),
                lease_id: existing.lease_id,
                interface: r.interface.clone(),
                attrs: r.attrs,
                transport: r.transport.map(|t| TransportConfig {
                    transport_type: t.r#type,
                    config: t.config,
                }),
            };

            self.index
                .update(entry.clone())
                .await
                .map_err(|e| Status::internal(e.to_string()))?;

            debug!(
                capability_id = %r.capability_id,
                interface = %r.interface,
                "service re-registered"
            );

            let _ = self.event_tx.send(RegistryChangedEvent {
                event_type: 2, // MODIFIED
                entry,
            });

            Ok(Response::new(RegisterResponse {
                capability_id: r.capability_id,
                lease: Some(Lease {
                    lease_id: lease.lease_id,
                    resource_id: lease.resource_id,
                    granted_at: Some(to_timestamp(lease.granted_at)),
                    expires_at: Some(to_timestamp(lease.expires_at)),
                    ttl_seconds: lease.ttl_seconds,
                }),
            }))
        } else {
            // New registration.
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

            Ok(Response::new(RegisterResponse {
                capability_id,
                lease: Some(Lease {
                    lease_id: lease.lease_id,
                    resource_id: lease.resource_id,
                    granted_at: Some(to_timestamp(lease.granted_at)),
                    expires_at: Some(to_timestamp(lease.expires_at)),
                    ttl_seconds: lease.ttl_seconds,
                }),
            }))
        }
    }

    async fn modify_attrs(
        &self,
        req: Request<ModifyAttrsRequest>,
    ) -> Result<Response<Capability>, Status> {
        let r = req.into_inner();

        let existing = self
            .index
            .get(&r.capability_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| {
                Status::not_found(format!("capability not found: {}", r.capability_id))
            })?;

        let mut attrs = existing.attrs.clone();
        // Remove first, then add — so adds override removes if same key appears in both.
        for key in &r.remove_attrs {
            attrs.remove(key);
        }
        for (k, v) in r.add_attrs {
            attrs.insert(k, v);
        }

        let entry = RegistryEntry {
            attrs,
            ..existing
        };

        self.index
            .update(entry.clone())
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        debug!(
            capability_id = %r.capability_id,
            "attributes modified"
        );

        let _ = self.event_tx.send(RegistryChangedEvent {
            event_type: 2, // MODIFIED
            entry: entry.clone(),
        });

        Ok(Response::new(entry_to_capability(&entry)))
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
                Ok(Response::new(entry_to_capability(&entry)))
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

        #[allow(clippy::result_large_err)] // tonic::Status is the gRPC error type — boxing it gains nothing
        let stream =
            tokio_stream::iter(entries.iter().map(|e| Ok(entry_to_capability(e))).collect::<Vec<_>>());
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
                            capability: Some(entry_to_capability(&evt.entry)),
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
