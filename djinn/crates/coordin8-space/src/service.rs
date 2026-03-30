use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};
use tracing::debug;

use coordin8_core::SpaceEventKind;
use coordin8_proto::coordin8::{
    space_service_server::SpaceService, CancelTupleRequest, Lease, OutRequest, OutResponse,
    Provenance, ReadRequest, ReadResponse, RenewTupleRequest, SpaceEvent, SpaceEventType,
    TakeRequest, TakeResponse, Tuple, WatchRequest,
};
use coordin8_registry::matcher::{matches, parse_template};

use crate::manager::SpaceManager;

fn to_timestamp(dt: chrono::DateTime<chrono::Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    }
}

fn tuple_record_to_proto(r: &coordin8_core::TupleRecord, lease: Option<Lease>) -> Tuple {
    Tuple {
        tuple_id: r.tuple_id.clone(),
        attrs: r.attrs.clone(),
        payload: r.payload.clone(),
        provenance: Some(Provenance {
            written_by: r.written_by.clone(),
            written_at: Some(to_timestamp(r.written_at)),
            input_tuple_id: r.input_tuple_id.clone().unwrap_or_default(),
        }),
        lease,
    }
}

pub struct SpaceServiceImpl {
    manager: Arc<SpaceManager>,
}

impl SpaceServiceImpl {
    pub fn new(manager: Arc<SpaceManager>) -> Self {
        Self { manager }
    }
}

type BoxStream<T> = Pin<Box<dyn futures_core::Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl SpaceService for SpaceServiceImpl {
    async fn out(&self, req: Request<OutRequest>) -> Result<Response<OutResponse>, Status> {
        let r = req.into_inner();
        let input_tuple_id = if r.input_tuple_id.is_empty() {
            None
        } else {
            Some(r.input_tuple_id)
        };

        let (record, lease_record) = self
            .manager
            .out(r.attrs, r.payload, r.ttl_seconds, r.written_by, input_tuple_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        debug!(tuple_id = %record.tuple_id, "out rpc");

        let lease = Some(Lease {
            lease_id: lease_record.lease_id,
            resource_id: lease_record.resource_id,
            granted_at: Some(to_timestamp(lease_record.granted_at)),
            expires_at: Some(to_timestamp(lease_record.expires_at)),
            ttl_seconds: lease_record.ttl_seconds,
        });

        Ok(Response::new(OutResponse {
            tuple: Some(tuple_record_to_proto(&record, lease)),
        }))
    }

    async fn read(&self, req: Request<ReadRequest>) -> Result<Response<ReadResponse>, Status> {
        let r = req.into_inner();
        let result = self
            .manager
            .read(r.template, r.wait, r.timeout_ms)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(ReadResponse {
            tuple: result.map(|rec| tuple_record_to_proto(&rec, None)),
        }))
    }

    async fn take(&self, req: Request<TakeRequest>) -> Result<Response<TakeResponse>, Status> {
        let r = req.into_inner();
        let result = self
            .manager
            .take(r.template, r.wait, r.timeout_ms)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(TakeResponse {
            tuple: result.map(|rec| tuple_record_to_proto(&rec, None)),
        }))
    }

    type WatchStream = BoxStream<SpaceEvent>;

    async fn watch(
        &self,
        req: Request<WatchRequest>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let r = req.into_inner();
        let on = match r.on {
            x if x == SpaceEventType::Expiration as i32 => SpaceEventKind::Expiration,
            _ => SpaceEventKind::Appearance,
        };

        let (_, _lease_id) = self
            .manager
            .watch(r.template.clone(), on.clone(), r.ttl_seconds)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        // Subscribe to the right broadcast BEFORE returning the stream.
        let broadcast_rx = match on {
            SpaceEventKind::Appearance => self.manager.subscribe_tuple_broadcast(),
            SpaceEventKind::Expiration => self.manager.subscribe_expiry_broadcast(),
        };

        let event_type = match on {
            SpaceEventKind::Appearance => SpaceEventType::Appearance as i32,
            SpaceEventKind::Expiration => SpaceEventType::Expiration as i32,
        };

        let template = r.template;
        let (tx, rx) = mpsc::channel::<Result<SpaceEvent, Status>>(64);

        tokio::spawn(async move {
            let ops = parse_template(&template);
            let mut stream = BroadcastStream::new(broadcast_rx);

            while let Some(result) = stream.next().await {
                let tuple = match result {
                    Ok(t) => t,
                    Err(_) => continue, // lagged — skip
                };

                if !ops.is_empty() && !matches(&ops, &tuple.attrs) {
                    continue;
                }

                let event = SpaceEvent {
                    r#type: event_type,
                    tuple: Some(tuple_record_to_proto(&tuple, None)),
                    occurred_at: Some(to_timestamp(chrono::Utc::now())),
                };

                if tx.send(Ok(event)).await.is_err() {
                    return; // client disconnected
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    async fn renew_tuple(
        &self,
        req: Request<RenewTupleRequest>,
    ) -> Result<Response<Lease>, Status> {
        let r = req.into_inner();
        let record = self
            .manager
            .renew_tuple(&r.tuple_id, r.ttl_seconds)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(Lease {
            lease_id: record.lease_id,
            resource_id: record.resource_id,
            granted_at: Some(to_timestamp(record.granted_at)),
            expires_at: Some(to_timestamp(record.expires_at)),
            ttl_seconds: record.ttl_seconds,
        }))
    }

    async fn cancel_tuple(
        &self,
        req: Request<CancelTupleRequest>,
    ) -> Result<Response<()>, Status> {
        let tuple_id = req.into_inner().tuple_id;
        self.manager
            .cancel_tuple(&tuple_id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(()))
    }
}
