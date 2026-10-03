use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};
use tracing::debug;
use uuid::Uuid;

use coordin8_core::{Leasing, RegistryEntry, TransportConfig};
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

/// Ownership check: the caller must present the entry's current `lease_id`
/// (only ever returned to the registrant in `RegisterResponse.lease`).
#[allow(clippy::result_large_err)]
fn check_owner(existing: &RegistryEntry, presented: &str) -> Result<(), Status> {
    if presented.is_empty() || presented != existing.lease_id {
        return Err(Status::permission_denied(
            "lease_id does not match the entry's current lease",
        ));
    }
    Ok(())
}

fn transport_eq(a: &Option<TransportConfig>, b: &Option<TransportConfig>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.transport_type == b.transport_type && a.config == b.config,
        _ => false,
    }
}

pub struct RegistryServiceImpl {
    index: Arc<RegistryIndex>,
    lease_manager: Arc<dyn Leasing>,
    event_tx: RegistryBroadcast,
    /// Stamped onto every `Lease` returned from `Register` so a holder
    /// always knows where to renew — Registry grants its own entries'
    /// leases in-process (see `.claude/plans/distributed-leasing/PRD.md`),
    /// so this is simply Registry's own listening address.
    grantor_host: String,
    grantor_port: u16,
    shutdown: coordin8_core::shutdown::ShutdownSignal,
}

impl RegistryServiceImpl {
    pub fn new(
        index: Arc<RegistryIndex>,
        lease_manager: Arc<dyn Leasing>,
        event_tx: RegistryBroadcast,
        grantor_host: impl Into<String>,
        grantor_port: u16,
    ) -> Self {
        Self {
            index,
            lease_manager,
            event_tx,
            grantor_host: grantor_host.into(),
            grantor_port,
            shutdown: Default::default(),
        }
    }

    /// End this service's long-lived server streams when `shutdown` fires
    /// (graceful drain), with `UNAVAILABLE` so client reconnect loops
    /// reconnect elsewhere instead of seeing a clean EOF.
    pub fn with_shutdown(mut self, shutdown: coordin8_core::shutdown::ShutdownSignal) -> Self {
        self.shutdown = shutdown;
        self
    }

    fn lease_to_proto(&self, lease: coordin8_core::LeaseRecord) -> Lease {
        Lease {
            lease_id: lease.lease_id,
            resource_id: lease.resource_id,
            granted_at: Some(to_timestamp(lease.granted_at)),
            expires_at: Some(to_timestamp(lease.expires_at)),
            ttl_seconds: lease.ttl_seconds,
            grantor_host: self.grantor_host.clone(),
            grantor_port: self.grantor_port as u32,
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

            check_owner(&existing, &r.lease_id)?;

            // Renew the existing lease.
            let lease = self
                .lease_manager
                .renew(&existing.lease_id, r.ttl_seconds)
                .await
                .map_err(|e| Status::failed_precondition(e.to_string()))?;

            let entry = RegistryEntry {
                capability_id: r.capability_id.clone(),
                lease_id: existing.lease_id.clone(),
                interface: r.interface.clone(),
                attrs: r.attrs,
                transport: r.transport.map(|t| TransportConfig {
                    transport_type: t.r#type,
                    config: t.config,
                }),
            };

            // A pure renewal (nothing but the lease changed) is not a
            // registry change: don't rewrite the entry or wake watchers.
            let changed = existing.interface != entry.interface
                || existing.attrs != entry.attrs
                || !transport_eq(&existing.transport, &entry.transport);

            if changed {
                self.index
                    .update(entry.clone())
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?;

                debug!(
                    capability_id = %r.capability_id,
                    interface = %r.interface,
                    "service re-registered with changes"
                );

                let _ = self.event_tx.send(RegistryChangedEvent {
                    event_type: 2, // MODIFIED
                    entry,
                });
            } else {
                debug!(capability_id = %r.capability_id, "registry entry renewed");
            }

            Ok(Response::new(RegisterResponse {
                capability_id: r.capability_id,
                lease: Some(self.lease_to_proto(lease)),
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
                lease: Some(self.lease_to_proto(lease)),
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

        check_owner(&existing, &r.lease_id)?;

        let mut attrs = existing.attrs.clone();
        // Remove first, then add — so adds override removes if same key appears in both.
        for key in &r.remove_attrs {
            attrs.remove(key);
        }
        for (k, v) in r.add_attrs {
            attrs.insert(k, v);
        }

        let entry = RegistryEntry { attrs, ..existing };

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

        #[allow(clippy::result_large_err)]
        // tonic::Status is the gRPC error type — boxing it gains nothing
        let stream = tokio_stream::iter(
            entries
                .iter()
                .map(|e| Ok(entry_to_capability(e)))
                .collect::<Vec<_>>(),
        );
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
                Err(BroadcastStreamRecvError::Lagged(_)) => Some(Err(Status::data_loss(
                    "watcher lagged; resubscribe and re-snapshot",
                ))),
            }
        });

        #[allow(clippy::result_large_err)] // tonic::Status is the gRPC error type
        let stream = self.shutdown.end_stream(Box::pin(stream), || {
            Err(Status::unavailable("server shutting down"))
        });
        Ok(Response::new(Box::pin(stream)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coordin8_core::LeaseConfig;
    use coordin8_lease::manager::LeaseManager;
    use coordin8_provider_local::InMemoryRegistryStore;
    use std::time::Duration;

    fn service() -> RegistryServiceImpl {
        let registry_store: Arc<dyn coordin8_core::RegistryStore> =
            Arc::new(InMemoryRegistryStore::default());
        let index = Arc::new(RegistryIndex::new(registry_store));
        let lease_manager: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
            Arc::new(coordin8_provider_local::InMemoryLeaseStore::default()),
            LeaseConfig::default(),
            broadcast::channel(16).0,
        ));
        RegistryServiceImpl::new(
            index,
            lease_manager,
            broadcast::channel(16).0,
            "127.0.0.1",
            9002,
        )
    }

    /// Regression test for a follow-up finding on the self-registration
    /// recovery fix (2026-09-07): a re-`Register` call on an entry whose
    /// lease has genuinely expired — but which is still present in the
    /// index, because nothing has reaped it yet — must surface
    /// `FailedPrecondition`, not `NotFound`. This is the code path
    /// `coordin8_bootstrap::self_register`'s renewal task now also treats as
    /// "entry unusable, re-register", alongside `NotFound`.
    ///
    /// Reproduced deterministically without a real reaper task or a Dynamo
    /// backend: this test constructs a `RegistryServiceImpl` directly (as
    /// production code does per-service, in `coordin8-djinn/src/services.rs`)
    /// with no reaper wired up, grants a real 1-second lease, and waits for
    /// it to actually expire — so the index still has the entry (nothing
    /// removed it), but `LeaseManager::renew` sees a genuinely expired
    /// record and returns `Error::LeaseExpired`.
    #[tokio::test]
    async fn reregister_on_an_entry_with_an_expired_lease_is_failed_precondition() {
        let svc = service();

        let initial = svc
            .register(Request::new(RegisterRequest {
                interface: "Whatever".to_string(),
                attrs: Default::default(),
                ttl_seconds: 1,
                transport: None,
                capability_id: String::new(),
                lease_id: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();

        tokio::time::sleep(Duration::from_millis(1100)).await;

        let err = svc
            .register(Request::new(RegisterRequest {
                interface: "Whatever".to_string(),
                attrs: Default::default(),
                ttl_seconds: 1,
                transport: None,
                capability_id: initial.capability_id,
                lease_id: initial.lease.unwrap().lease_id,
            }))
            .await
            .unwrap_err();

        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    fn service_with_events(capacity: usize) -> (RegistryServiceImpl, RegistryBroadcast) {
        let registry_store: Arc<dyn coordin8_core::RegistryStore> =
            Arc::new(InMemoryRegistryStore::default());
        let index = Arc::new(RegistryIndex::new(registry_store));
        let lease_manager: Arc<dyn Leasing> = Arc::new(LeaseManager::new(
            Arc::new(coordin8_provider_local::InMemoryLeaseStore::default()),
            LeaseConfig::default(),
            broadcast::channel(16).0,
        ));
        let tx = broadcast::channel(capacity).0;
        (
            RegistryServiceImpl::new(index, lease_manager, tx.clone(), "127.0.0.1", 9002),
            tx,
        )
    }

    fn reg_req(attrs: &[(&str, &str)], cap: &str, lease: &str) -> RegisterRequest {
        RegisterRequest {
            interface: "Svc".to_string(),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ttl_seconds: 30,
            transport: Some(TransportDescriptor {
                r#type: "grpc".to_string(),
                config: [("host".to_string(), "good".to_string())].into(),
            }),
            capability_id: cap.to_string(),
            lease_id: lease.to_string(),
        }
    }

    async fn lookup_attrs(svc: &RegistryServiceImpl) -> Capability {
        svc.lookup(Request::new(LookupRequest {
            template: [("interface".to_string(), "Svc".to_string())].into(),
        }))
        .await
        .unwrap()
        .into_inner()
    }

    #[tokio::test]
    async fn reregister_requires_the_entrys_lease_id() {
        let (svc, _tx) = service_with_events(16);
        let first = svc
            .register(Request::new(reg_req(&[("a", "1")], "", "")))
            .await
            .unwrap()
            .into_inner();
        let cap = first.capability_id.clone();
        let lease = first.lease.unwrap().lease_id;

        // Hijack attempts: wrong and missing lease_id.
        for bad in ["wrong-lease", ""] {
            let mut evil = reg_req(&[("a", "evil")], &cap, bad);
            evil.transport.as_mut().unwrap().config =
                [("host".to_string(), "evil".to_string())].into();
            let err = svc.register(Request::new(evil)).await.unwrap_err();
            assert_eq!(err.code(), tonic::Code::PermissionDenied);
        }
        let seen = lookup_attrs(&svc).await;
        assert_eq!(seen.attrs.get("a").map(String::as_str), Some("1"));
        assert_eq!(seen.transport.unwrap().config["host"], "good");

        // The owner can still re-register.
        svc.register(Request::new(reg_req(&[("a", "2")], &cap, &lease)))
            .await
            .unwrap();
        assert_eq!(
            lookup_attrs(&svc).await.attrs.get("a").map(String::as_str),
            Some("2")
        );
    }

    #[tokio::test]
    async fn modify_attrs_requires_the_entrys_lease_id() {
        let (svc, _tx) = service_with_events(16);
        let first = svc
            .register(Request::new(reg_req(&[("a", "1")], "", "")))
            .await
            .unwrap()
            .into_inner();
        let cap = first.capability_id.clone();
        let lease = first.lease.unwrap().lease_id;

        for bad in ["wrong-lease", ""] {
            let err = svc
                .modify_attrs(Request::new(ModifyAttrsRequest {
                    capability_id: cap.clone(),
                    add_attrs: [("a".to_string(), "evil".to_string())].into(),
                    remove_attrs: vec![],
                    lease_id: bad.to_string(),
                }))
                .await
                .unwrap_err();
            assert_eq!(err.code(), tonic::Code::PermissionDenied);
        }
        assert_eq!(
            lookup_attrs(&svc).await.attrs.get("a").map(String::as_str),
            Some("1")
        );

        let updated = svc
            .modify_attrs(Request::new(ModifyAttrsRequest {
                capability_id: cap,
                add_attrs: [("a".to_string(), "2".to_string())].into(),
                remove_attrs: vec![],
                lease_id: lease,
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(updated.attrs.get("a").map(String::as_str), Some("2"));
    }

    #[tokio::test]
    async fn pure_renewal_emits_no_modified_but_a_change_does() {
        let (svc, tx) = service_with_events(16);
        let first = svc
            .register(Request::new(reg_req(&[("a", "1")], "", "")))
            .await
            .unwrap()
            .into_inner();
        let cap = first.capability_id.clone();
        let lease = first.lease.unwrap().lease_id;
        let mut rx = tx.subscribe();

        // Identical re-register: renewal only.
        svc.register(Request::new(reg_req(&[("a", "1")], &cap, &lease)))
            .await
            .unwrap();
        assert!(
            rx.try_recv().is_err(),
            "pure renewal must not emit MODIFIED"
        );

        // Real change: exactly one MODIFIED.
        svc.register(Request::new(reg_req(&[("a", "2")], &cap, &lease)))
            .await
            .unwrap();
        let evt = rx.try_recv().expect("attr change must emit MODIFIED");
        assert_eq!(evt.event_type, 2);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn watch_that_lags_ends_with_data_loss() {
        let (svc, tx) = service_with_events(2);
        let mut stream = svc
            .watch(Request::new(RegistryWatchRequest {
                template: Default::default(),
            }))
            .await
            .unwrap()
            .into_inner();

        let entry = RegistryEntry {
            capability_id: "c".into(),
            lease_id: "l".into(),
            interface: "Svc".into(),
            attrs: Default::default(),
            transport: None,
        };
        for _ in 0..10 {
            let _ = tx.send(RegistryChangedEvent {
                event_type: 0,
                entry: entry.clone(),
            });
        }

        let mut saw_loss = false;
        while let Some(item) = stream.next().await {
            if let Err(status) = item {
                assert_eq!(status.code(), tonic::Code::DataLoss);
                saw_loss = true;
                break;
            }
        }
        assert!(saw_loss, "lagged watcher must receive DATA_LOSS");
    }
}
