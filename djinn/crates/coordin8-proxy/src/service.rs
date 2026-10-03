use std::sync::Arc;

use tonic::{Request, Response, Status};
use tracing::debug;

use coordin8_proto::coordin8::proxy_service_server::ProxyService;
use coordin8_proto::coordin8::{Lease, OpenRequest, ProxyHandle, ReleaseRequest};

use crate::manager::{ProxyError, ProxyManager};

/// Map a proxy error to a gRPC status. A `Store` error wrapping
/// `Unavailable` gets its own code (the dependency isn't ready yet, safe to
/// retry) rather than falling into the generic `not_found` bucket used for
/// everything else here.
fn map_err(e: ProxyError) -> Status {
    match e {
        ProxyError::Store(coordin8_core::error::Error::Unavailable(_)) => {
            Status::unavailable(e.to_string())
        }
        _ => Status::not_found(e.to_string()),
    }
}

fn to_timestamp(dt: chrono::DateTime<chrono::Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: dt.timestamp(),
        nanos: dt.timestamp_subsec_nanos() as i32,
    }
}

pub struct ProxyServiceImpl {
    manager: Arc<ProxyManager>,
    /// Stamped onto every `Lease` returned from `Open` so a holder knows where
    /// to renew — Proxy is a Landlord for its own leases, so this is simply
    /// Proxy's own listening address.
    grantor_host: String,
    grantor_port: u16,
}

impl ProxyServiceImpl {
    pub fn new(
        manager: Arc<ProxyManager>,
        grantor_host: impl Into<String>,
        grantor_port: u16,
    ) -> Self {
        Self {
            manager,
            grantor_host: grantor_host.into(),
            grantor_port,
        }
    }
}

#[tonic::async_trait]
impl ProxyService for ProxyServiceImpl {
    async fn open(&self, request: Request<OpenRequest>) -> Result<Response<ProxyHandle>, Status> {
        let req = request.into_inner();
        let (template, ttl_seconds) = (req.template, req.ttl_seconds);
        debug!(?template, ttl_seconds, "proxy open request");

        let (proxy_id, local_port, lease) = self
            .manager
            .open(template, ttl_seconds)
            .await
            .map_err(map_err)?;

        Ok(Response::new(ProxyHandle {
            proxy_id,
            local_port: local_port as i32,
            lease: Some(Lease {
                lease_id: lease.lease_id,
                resource_id: lease.resource_id,
                granted_at: Some(to_timestamp(lease.granted_at)),
                expires_at: Some(to_timestamp(lease.expires_at)),
                ttl_seconds: lease.ttl_seconds,
                grantor_host: self.grantor_host.clone(),
                grantor_port: self.grantor_port as u32,
            }),
        }))
    }

    async fn release(&self, request: Request<ReleaseRequest>) -> Result<Response<()>, Status> {
        let proxy_id = request.into_inner().proxy_id;
        debug!(%proxy_id, "proxy release request");

        self.manager.close(&proxy_id).await.map_err(map_err)?;

        Ok(Response::new(()))
    }
}
