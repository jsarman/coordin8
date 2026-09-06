use std::sync::Arc;

use tonic::{Request, Response, Status};
use tracing::debug;

use coordin8_core::TransactionState;
use coordin8_proto::coordin8::{
    transaction_service_server::TransactionService, AbortRequest, BeginRequest, CommitRequest,
    EnlistRequest, GetStateRequest, Lease, TransactionCreated, TransactionStateResponse,
};

use crate::manager::TxnManager;

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

fn state_to_proto(s: &TransactionState) -> i32 {
    match s {
        TransactionState::Active => 0,
        TransactionState::Voting => 1,
        TransactionState::Prepared => 2,
        TransactionState::Committed => 3,
        TransactionState::Aborted => 4,
    }
}

pub struct TxnServiceImpl {
    manager: Arc<TxnManager>,
    /// Stamped onto every `Lease` returned from `Begin` so a holder always
    /// knows where to renew — TxnMgr grants its own transaction leases
    /// in-process (see `.claude/plans/distributed-leasing/PRD.md`), so this
    /// is simply TxnMgr's own listening address.
    grantor_host: String,
    grantor_port: u16,
}

impl TxnServiceImpl {
    pub fn new(
        manager: Arc<TxnManager>,
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
impl TransactionService for TxnServiceImpl {
    async fn begin(
        &self,
        req: Request<BeginRequest>,
    ) -> Result<Response<TransactionCreated>, Status> {
        let r = req.into_inner();
        let (txn_id, lease) = self.manager.begin(r.ttl_seconds).await.map_err(map_err)?;

        debug!(txn_id, "begin rpc");

        Ok(Response::new(TransactionCreated {
            txn_id,
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

    async fn enlist(&self, req: Request<EnlistRequest>) -> Result<Response<()>, Status> {
        let r = req.into_inner();
        self.manager
            .enlist(&r.txn_id, r.participant_endpoint, r.crash_count)
            .await
            .map_err(map_err)?;
        Ok(Response::new(()))
    }

    async fn get_state(
        &self,
        req: Request<GetStateRequest>,
    ) -> Result<Response<TransactionStateResponse>, Status> {
        let txn_id = req.into_inner().txn_id;
        let state = self
            .manager
            .get_state(&txn_id)
            .await
            .map_err(|e| Status::not_found(e.to_string()))?;

        Ok(Response::new(TransactionStateResponse {
            state: state_to_proto(&state),
        }))
    }

    async fn commit(&self, req: Request<CommitRequest>) -> Result<Response<()>, Status> {
        let r = req.into_inner();
        // v1: wait_millis ignored — all commits are synchronous
        self.manager
            .commit(&r.txn_id)
            .await
            .map_err(|e| Status::aborted(e.to_string()))?;
        Ok(Response::new(()))
    }

    async fn abort(&self, req: Request<AbortRequest>) -> Result<Response<()>, Status> {
        let r = req.into_inner();
        self.manager.abort(&r.txn_id).await.map_err(map_err)?;
        Ok(Response::new(()))
    }
}
