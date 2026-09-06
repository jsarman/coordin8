use std::pin::Pin;
use std::sync::Arc;

use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use coordin8_core::{LeaseReclaimed, LeaseRecord, Leasing, ReclaimReason};
use coordin8_proto::coordin8::{
    lease_service_server::LeaseService, renew_all_request, renew_all_response, CancelRequest,
    ExpiryEvent, GrantRequest, Lease, ReclaimReason as ProtoReclaimReason, RenewAllRequest,
    RenewAllResponse, RenewRequest, WatchExpiryRequest,
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

/// A `LeaseService` gRPC endpoint, mounted alongside a service's own primary
/// RPC service (e.g. `RegistryService` + `LeaseService` both on Registry's
/// port). `grantor_host`/`grantor_port` are stamped onto every returned
/// `Lease` so a holder always knows where to renew — see the proto's doc
/// comment on `Lease` for why.
pub struct LeaseServiceImpl {
    manager: Arc<LeaseManager>,
    expiry_tx: ExpiryBroadcast,
    grantor_host: String,
    grantor_port: u16,
}

impl LeaseServiceImpl {
    pub fn new(
        manager: Arc<LeaseManager>,
        expiry_tx: ExpiryBroadcast,
        grantor_host: impl Into<String>,
        grantor_port: u16,
    ) -> Self {
        Self {
            manager,
            expiry_tx,
            grantor_host: grantor_host.into(),
            grantor_port,
        }
    }

    fn record_to_proto(&self, r: LeaseRecord) -> Lease {
        Lease {
            lease_id: r.lease_id,
            resource_id: r.resource_id,
            granted_at: to_timestamp(r.granted_at),
            expires_at: to_timestamp(r.expires_at),
            ttl_seconds: r.ttl_seconds,
            grantor_host: self.grantor_host.clone(),
            grantor_port: self.grantor_port as u32,
        }
    }
}

type BoxStream<T> = Pin<Box<dyn futures_core::Stream<Item = Result<T, Status>> + Send + 'static>>;

fn renew_status(e: coordin8_core::Error) -> Status {
    match e {
        coordin8_core::Error::LeaseNotFound(_) => Status::not_found(e.to_string()),
        coordin8_core::Error::LeaseExpired(_) => Status::failed_precondition(e.to_string()),
        _ => Status::internal(e.to_string()),
    }
}

#[tonic::async_trait]
impl LeaseService for LeaseServiceImpl {
    async fn grant(&self, req: Request<GrantRequest>) -> Result<Response<Lease>, Status> {
        let r = req.into_inner();
        self.manager
            .grant(&r.resource_id, r.ttl_seconds)
            .await
            .map(|record| Response::new(self.record_to_proto(record)))
            .map_err(|e| Status::internal(e.to_string()))
    }

    async fn renew(&self, req: Request<RenewRequest>) -> Result<Response<Lease>, Status> {
        let r = req.into_inner();
        self.manager
            .renew(&r.lease_id, r.ttl_seconds)
            .await
            .map(|record| Response::new(self.record_to_proto(record)))
            .map_err(renew_status)
    }

    async fn renew_all(
        &self,
        req: Request<RenewAllRequest>,
    ) -> Result<Response<RenewAllResponse>, Status> {
        let r = req.into_inner();
        let mut results = Vec::with_capacity(r.leases.len());
        for renew_all_request::Item {
            lease_id,
            ttl_seconds,
        } in r.leases
        {
            match self.manager.renew(&lease_id, ttl_seconds).await {
                Ok(record) => results.push(renew_all_response::Result {
                    lease_id,
                    lease: Some(self.record_to_proto(record)),
                    error_message: String::new(),
                }),
                Err(e) => results.push(renew_all_response::Result {
                    lease_id,
                    lease: None,
                    error_message: e.to_string(),
                }),
            }
        }
        Ok(Response::new(RenewAllResponse { results }))
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
                Ok(LeaseReclaimed { record, reason }) => {
                    if filter.is_empty() || filter == record.resource_id {
                        let reason = match reason {
                            ReclaimReason::Expired => ProtoReclaimReason::Expired,
                            ReclaimReason::Cancelled => ProtoReclaimReason::Cancelled,
                        };
                        Some(Ok(ExpiryEvent {
                            lease_id: record.lease_id,
                            resource_id: record.resource_id,
                            expired_at: to_timestamp(record.expires_at),
                            reason: reason as i32,
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
