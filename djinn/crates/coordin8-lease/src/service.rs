use std::pin::Pin;
use std::sync::Arc;

use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use coordin8_core::{LeaseRecord, Leasing};
use coordin8_proto::coordin8::{
    lease_service_server::LeaseService, CancelRequest, ExpiryEvent, GrantRequest, Lease,
    RenewRequest, WatchExpiryRequest,
};
use prost_types::Timestamp;

use crate::manager::LeaseManager;
use crate::reaper::ExpiryBroadcast;

fn to_timestamp(dt: chrono::DateTime<chrono::Utc>) -> Option<Timestamp> {
    Some(Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    })
}

fn record_to_proto(r: LeaseRecord) -> Lease {
    Lease {
        lease_id: r.lease_id,
        resource_id: r.resource_id,
        granted_at: to_timestamp(r.granted_at),
        expires_at: to_timestamp(r.expires_at),
        ttl_seconds: r.ttl_seconds,
    }
}

pub struct LeaseServiceImpl {
    manager: Arc<LeaseManager>,
    expiry_tx: ExpiryBroadcast,
}

impl LeaseServiceImpl {
    pub fn new(manager: Arc<LeaseManager>, expiry_tx: ExpiryBroadcast) -> Self {
        Self { manager, expiry_tx }
    }
}

type BoxStream<T> = Pin<Box<dyn futures_core::Stream<Item = Result<T, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl LeaseService for LeaseServiceImpl {
    async fn grant(&self, req: Request<GrantRequest>) -> Result<Response<Lease>, Status> {
        let r = req.into_inner();
        self.manager
            .grant(&r.resource_id, r.ttl_seconds)
            .await
            .map(|record| Response::new(record_to_proto(record)))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn renew(&self, req: Request<RenewRequest>) -> Result<Response<Lease>, Status> {
        let r = req.into_inner();
        self.manager
            .renew(&r.lease_id, r.ttl_seconds)
            .await
            .map(|record| Response::new(record_to_proto(record)))
            .map_err(|e| match e {
                coordin8_core::Error::LeaseNotFound(_) => Status::not_found(e.to_string()),
                coordin8_core::Error::LeaseExpired(_) => Status::failed_precondition(e.to_string()),
                _ => Status::internal(e.to_string()),
            })
    }

    // google.protobuf.Empty maps to () in tonic-generated Rust code.
    async fn cancel(&self, req: Request<CancelRequest>) -> Result<Response<()>, Status> {
        let r = req.into_inner();
        self.manager
            .cancel(&r.lease_id)
            .await
            .map(|_| Response::new(()))
            .map_err(|e| Status::internal(e.to_string()))
    }

    type WatchExpiryStream = BoxStream<ExpiryEvent>;

    async fn watch_expiry(
        &self,
        req: Request<WatchExpiryRequest>,
    ) -> Result<Response<Self::WatchExpiryStream>, Status> {
        let filter_resource = req.into_inner().resource_id;
        let rx = self.expiry_tx.subscribe();

        let stream = BroadcastStream::new(rx).filter_map(move |result| {
            let filter = filter_resource.clone();
            match result {
                Ok(record) => {
                    if filter.is_empty() || filter == record.resource_id {
                        Some(Ok(ExpiryEvent {
                            lease_id: record.lease_id,
                            resource_id: record.resource_id,
                            expired_at: to_timestamp(record.expires_at),
                        }))
                    } else {
                        None
                    }
                }
                Err(_) => None, // lagged — skip
            }
        });

        Ok(Response::new(Box::pin(stream)))
    }
}
