use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};
use tracing::debug;

use coordin8_core::DeliveryMode;
use coordin8_proto::coordin8::{
    event_service_server::EventService, CancelSubscriptionRequest, EmitRequest, Event,
    EventRegistration, Lease, ReceiveRequest, RenewSubscriptionRequest, SubscribeRequest,
};

use crate::manager::EventManager;

/// Map a core error to a gRPC status. `Unavailable` gets its own code (the
/// dependency isn't ready yet, safe to retry) rather than falling into the
/// generic `internal` bucket.
fn map_err(e: coordin8_core::Error) -> Status {
    match e {
        coordin8_core::Error::Unavailable(_) => Status::unavailable(e.to_string()),
        _ => Status::internal(e.to_string()),
    }
}

fn to_timestamp(dt: chrono::DateTime<chrono::Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    }
}

fn event_record_to_proto(e: &coordin8_core::EventRecord, handback: &[u8]) -> Event {
    Event {
        event_id: e.event_id.clone(),
        source: e.source.clone(),
        event_type: e.event_type.clone(),
        seq_num: e.seq_num,
        attrs: e.attrs.clone(),
        payload: e.payload.clone(),
        handback: handback.to_vec(),
        emitted_at: Some(to_timestamp(e.emitted_at)),
    }
}

pub struct EventServiceImpl {
    manager: Arc<EventManager>,
    /// Stamped onto every `Lease` returned from `Subscribe`/`RenewSubscription`
    /// so a holder always knows where to renew — EventMgr grants its own
    /// subscription leases in-process (see
    /// `.claude/plans/distributed-leasing/PRD.md`), so this is simply
    /// EventMgr's own listening address.
    grantor_host: String,
    grantor_port: u16,
}

impl EventServiceImpl {
    pub fn new(
        manager: Arc<EventManager>,
        grantor_host: impl Into<String>,
        grantor_port: u16,
    ) -> Self {
        Self {
            manager,
            grantor_host: grantor_host.into(),
            grantor_port,
        }
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
impl EventService for EventServiceImpl {
    async fn subscribe(
        &self,
        req: Request<SubscribeRequest>,
    ) -> Result<Response<EventRegistration>, Status> {
        let r = req.into_inner();
        let delivery = match r.delivery {
            1 => DeliveryMode::BestEffort,
            _ => DeliveryMode::Durable,
        };

        let (registration_id, lease, seq_num) = self
            .manager
            .subscribe(
                r.source.clone(),
                r.template,
                delivery,
                r.ttl_seconds,
                r.handback,
            )
            .await
            .map_err(map_err)?;

        debug!(registration_id, source = %r.source, "subscribe rpc");

        Ok(Response::new(EventRegistration {
            registration_id,
            source: r.source,
            lease: Some(self.lease_to_proto(lease)),
            seq_num,
        }))
    }

    type ReceiveStream = BoxStream<Event>;

    async fn receive(
        &self,
        req: Request<ReceiveRequest>,
    ) -> Result<Response<Self::ReceiveStream>, Status> {
        let registration_id = req.into_inner().registration_id;

        let sub = self
            .manager
            .get_subscription(&registration_id)
            .await
            .map_err(map_err)?
            .ok_or_else(|| Status::not_found("subscription not found"))?;

        let handback = sub.handback.clone();
        let source = sub.source.clone();
        let template = sub.template.clone();
        let is_durable = sub.delivery == DeliveryMode::Durable;

        // Subscribe to broadcast BEFORE draining mailbox to avoid missing events
        // in the gap. Clients use seq_num to deduplicate.
        let broadcast_rx = self.manager.subscribe_broadcast();

        let backlog = if is_durable {
            self.manager
                .drain_mailbox(&registration_id)
                .await
                .unwrap_or_default()
        } else {
            vec![]
        };

        debug!(
            registration_id,
            backlog = backlog.len(),
            "receive rpc started"
        );

        let (tx, rx) = mpsc::channel::<Result<Event, Status>>(64);

        tokio::spawn(async move {
            // Send backlog first.
            for event in &backlog {
                let proto_event = event_record_to_proto(event, &handback);
                if tx.send(Ok(proto_event)).await.is_err() {
                    return;
                }
            }

            // Then stream live events, filtered to this subscription.
            let ops = coordin8_registry::matcher::parse_template(&template);
            let mut stream = BroadcastStream::new(broadcast_rx);

            while let Some(result) = stream.next().await {
                let event = match result {
                    Ok(e) => e,
                    Err(_) => continue, // lagged receiver — skip
                };

                if event.source != source {
                    continue;
                }

                let mut check_attrs = event.attrs.clone();
                check_attrs.insert("event_type".to_string(), event.event_type.clone());
                if !ops.is_empty() && !coordin8_registry::matcher::matches(&ops, &check_attrs) {
                    continue;
                }

                let proto_event = event_record_to_proto(&event, &handback);
                if tx.send(Ok(proto_event)).await.is_err() {
                    return;
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn emit(&self, req: Request<EmitRequest>) -> Result<Response<()>, Status> {
        let r = req.into_inner();
        self.manager
            .emit(r.source, r.event_type, r.attrs, r.payload)
            .await
            .map_err(map_err)?;

        Ok(Response::new(()))
    }

    async fn renew_subscription(
        &self,
        req: Request<RenewSubscriptionRequest>,
    ) -> Result<Response<Lease>, Status> {
        let r = req.into_inner();
        let record = self
            .manager
            .renew_subscription(&r.registration_id, r.ttl_seconds)
            .await
            .map_err(map_err)?;

        Ok(Response::new(self.lease_to_proto(record)))
    }

    async fn cancel_subscription(
        &self,
        req: Request<CancelSubscriptionRequest>,
    ) -> Result<Response<()>, Status> {
        let registration_id = req.into_inner().registration_id;
        self.manager
            .cancel_subscription(&registration_id)
            .await
            .map_err(map_err)?;

        Ok(Response::new(()))
    }
}
