use std::sync::Arc;

use tonic::{Request, Response, Status};
use tracing::debug;

use coordin8_proto::coordin8::{
    participant_service_server::ParticipantService, ParticipantRequest, PrepareResponse,
};

use crate::manager::SpaceManager;

/// Map a core error to a gRPC status. `Unavailable` gets its own code (the
/// dependency isn't ready yet, safe to retry) rather than falling into the
/// generic `internal` bucket.
fn map_err(e: coordin8_core::Error) -> Status {
    match e {
        coordin8_core::Error::Unavailable(_) => Status::unavailable(e.to_string()),
        _ => Status::internal(e.to_string()),
    }
}

/// ParticipantService implementation for the Space.
///
/// When the Space is enlisted in a transaction (via write/take with txn_id),
/// the 2PC coordinator calls back to these methods during commit/abort.
///
///   Prepare        → PREPARED (buffered tuples ready to flush)
///   Commit         → flush uncommitted writes, finalize takes
///   Abort          → discard uncommitted writes, restore takes
///   PrepareAndCommit → single-participant optimization
pub struct SpaceParticipantService {
    manager: Arc<SpaceManager>,
}

impl SpaceParticipantService {
    pub fn new(manager: Arc<SpaceManager>) -> Self {
        Self { manager }
    }
}

#[tonic::async_trait]
impl ParticipantService for SpaceParticipantService {
    async fn prepare(
        &self,
        req: Request<ParticipantRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        let txn_id = req.into_inner().txn_id;

        // If we have uncommitted state, vote PREPARED.
        // If no state (read-only), vote NOTCHANGED.
        let has_state = self.manager.has_txn(&txn_id).await.map_err(map_err)?;

        let vote = if has_state {
            debug!(txn_id, "space participant: PREPARED");
            0 // VOTE_PREPARED
        } else {
            debug!(txn_id, "space participant: NOTCHANGED");
            1 // VOTE_NOTCHANGED
        };

        Ok(Response::new(PrepareResponse { vote }))
    }

    async fn commit(&self, req: Request<ParticipantRequest>) -> Result<Response<()>, Status> {
        let txn_id = req.into_inner().txn_id;

        self.manager
            .commit_space_txn(&txn_id)
            .await
            .map_err(map_err)?;

        debug!(txn_id, "space participant: committed");
        Ok(Response::new(()))
    }

    async fn abort(&self, req: Request<ParticipantRequest>) -> Result<Response<()>, Status> {
        let txn_id = req.into_inner().txn_id;

        self.manager
            .abort_space_txn(&txn_id)
            .await
            .map_err(map_err)?;

        debug!(txn_id, "space participant: aborted");
        Ok(Response::new(()))
    }

    async fn prepare_and_commit(
        &self,
        req: Request<ParticipantRequest>,
    ) -> Result<Response<PrepareResponse>, Status> {
        let txn_id = req.into_inner().txn_id;

        let has_state = self.manager.has_txn(&txn_id).await.map_err(map_err)?;

        if has_state {
            self.manager
                .commit_space_txn(&txn_id)
                .await
                .map_err(map_err)?;

            debug!(txn_id, "space participant: prepare-and-commit (PREPARED)");
            Ok(Response::new(PrepareResponse { vote: 0 }))
        } else {
            debug!(txn_id, "space participant: prepare-and-commit (NOTCHANGED)");
            Ok(Response::new(PrepareResponse { vote: 1 }))
        }
    }
}
