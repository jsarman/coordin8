use std::sync::Arc;

use tonic::{Request, Response, Status};
use tracing::debug;

use coordin8_proto::coordin8::proxy_service_server::ProxyService;
use coordin8_proto::coordin8::{OpenRequest, ProxyHandle, ReleaseRequest};

use crate::manager::ProxyManager;

pub struct ProxyServiceImpl {
    manager: Arc<ProxyManager>,
}

impl ProxyServiceImpl {
    pub fn new(manager: Arc<ProxyManager>) -> Self {
        Self { manager }
    }
}

#[tonic::async_trait]
impl ProxyService for ProxyServiceImpl {
    async fn open(&self, request: Request<OpenRequest>) -> Result<Response<ProxyHandle>, Status> {
        let template = request.into_inner().template;
        debug!(?template, "proxy open request");

        let (proxy_id, local_port) = self
            .manager
            .open(template)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(ProxyHandle {
            proxy_id,
            local_port: local_port as i32,
        }))
    }

    async fn release(&self, request: Request<ReleaseRequest>) -> Result<Response<()>, Status> {
        let proxy_id = request.into_inner().proxy_id;
        debug!(%proxy_id, "proxy release request");

        self.manager
            .close(&proxy_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(()))
    }
}
