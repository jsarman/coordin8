use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::future::join_all;
use tracing::{debug, error, warn};
use uuid::Uuid;

use coordin8_core::{
    Error, LeaseRecord, ParticipantRecord, PrepareVote, TransactionRecord, TransactionState,
    TxnStore,
};
use coordin8_lease::LeaseManager;
use coordin8_proto::coordin8::{
    participant_service_client::ParticipantServiceClient, ParticipantRequest,
};

pub struct TxnManager {
    store: Arc<dyn TxnStore>,
    lease_manager: Arc<LeaseManager>,
}

impl TxnManager {
    pub fn new(store: Arc<dyn TxnStore>, lease_manager: Arc<LeaseManager>) -> Self {
        Self {
            store,
            lease_manager,
        }
    }

    // ── Public API ────────────────────────────────────────────────────────────

    pub async fn begin(&self, ttl_secs: u64) -> Result<(String, LeaseRecord), Error> {
        let txn_id = Uuid::new_v4().to_string();
        let resource_id = format!("txn:{}", txn_id);
        let lease = self.lease_manager.grant(&resource_id, ttl_secs).await?;

        let record = TransactionRecord {
            txn_id: txn_id.clone(),
            lease_id: lease.lease_id.clone(),
            state: TransactionState::Active,
            participants: vec![],
            begun_at: Utc::now(),
        };

        self.store.create(record).await?;
        debug!(txn_id, lease_id = %lease.lease_id, ttl_secs, "transaction begun");
        Ok((txn_id, lease))
    }

    pub async fn enlist(
        &self,
        txn_id: &str,
        endpoint: String,
        crash_count: u64,
    ) -> Result<(), Error> {
        let txn = self
            .store
            .get(txn_id)
            .await?
            .ok_or_else(|| Error::TransactionNotFound(txn_id.to_string()))?;

        if txn.state != TransactionState::Active {
            return Err(Error::TransactionTerminal(txn_id.to_string()));
        }

        let participant = ParticipantRecord {
            endpoint: endpoint.clone(),
            crash_count,
        };
        self.store.add_participant(txn_id, participant).await?;
        debug!(txn_id, endpoint, crash_count, "participant enlisted");
        Ok(())
    }

    pub async fn get_state(&self, txn_id: &str) -> Result<TransactionState, Error> {
        let txn = self
            .store
            .get(txn_id)
            .await?
            .ok_or_else(|| Error::TransactionNotFound(txn_id.to_string()))?;
        Ok(txn.state)
    }

    /// Abort a transaction due to lease expiry. Does not cancel the lease
    /// (it's already expired). Called by the lease expiry cascade.
    pub async fn abort_expired(&self, txn_id: &str) -> Result<(), Error> {
        let txn = match self.store.get(txn_id).await? {
            Some(t) => t,
            None => return Ok(()), // already cleaned up
        };

        match txn.state {
            TransactionState::Committed | TransactionState::Aborted => return Ok(()),
            _ => {}
        }

        self.store
            .update_state(txn_id, TransactionState::Aborted)
            .await?;
        Self::do_abort_participants(txn_id, &txn.participants).await;
        debug!(txn_id, "transaction aborted (lease expired)");
        Ok(())
    }

    pub async fn abort(&self, txn_id: &str) -> Result<(), Error> {
        let txn = self
            .store
            .get(txn_id)
            .await?
            .ok_or_else(|| Error::TransactionNotFound(txn_id.to_string()))?;

        match txn.state {
            TransactionState::Committed => {
                return Err(Error::TransactionTerminal(txn_id.to_string()));
            }
            TransactionState::Aborted => return Ok(()), // idempotent
            _ => {}
        }

        self.store
            .update_state(txn_id, TransactionState::Aborted)
            .await?;
        Self::do_abort_participants(txn_id, &txn.participants).await;
        self.lease_manager.cancel(&txn.lease_id).await.ok();
        debug!(txn_id, "transaction aborted");
        Ok(())
    }

    pub async fn commit(&self, txn_id: &str) -> Result<(), Error> {
        let txn = self
            .store
            .get(txn_id)
            .await?
            .ok_or_else(|| Error::TransactionNotFound(txn_id.to_string()))?;

        // ── Terminal state guards ─────────────────────────────────────────────
        match txn.state {
            TransactionState::Committed => return Ok(()), // idempotent
            TransactionState::Aborted => return Err(Error::TransactionAborted(txn_id.to_string())),
            _ => {}
        }

        let participants = txn.participants.clone();

        // ── Zero participants: trivially committed ────────────────────────────
        if participants.is_empty() {
            self.store
                .update_state(txn_id, TransactionState::Committed)
                .await?;
            self.lease_manager.cancel(&txn.lease_id).await.ok();
            debug!(txn_id, "zero-participant commit");
            return Ok(());
        }

        // ── Single-participant optimization: PrepareAndCommit ─────────────────
        if participants.len() == 1 {
            let ep = &participants[0].endpoint;
            self.store
                .update_state(txn_id, TransactionState::Voting)
                .await?;
            match Self::call_prepare_and_commit(ep, txn_id).await {
                Ok(PrepareVote::Prepared) | Ok(PrepareVote::NotChanged) => {
                    self.store
                        .update_state(txn_id, TransactionState::Committed)
                        .await?;
                    self.lease_manager.cancel(&txn.lease_id).await.ok();
                    debug!(txn_id, "single-participant commit via PrepareAndCommit");
                    return Ok(());
                }
                Ok(PrepareVote::Aborted) | Err(_) => {
                    self.store
                        .update_state(txn_id, TransactionState::Aborted)
                        .await?;
                    self.lease_manager.cancel(&txn.lease_id).await.ok();
                    warn!(txn_id, endpoint = %ep, "single participant voted ABORTED or failed");
                    return Err(Error::TransactionAborted(txn_id.to_string()));
                }
            }
        }

        // ── Phase 1: Prepare (parallel) ───────────────────────────────────────
        self.store
            .update_state(txn_id, TransactionState::Voting)
            .await?;

        let prepare_futs: Vec<_> = participants
            .iter()
            .map(|p| {
                let ep = p.endpoint.clone();
                let tid = txn_id.to_string();
                async move {
                    let result = Self::call_prepare(&ep, &tid).await;
                    (ep, result)
                }
            })
            .collect();

        let prepare_results = join_all(prepare_futs).await;

        let mut prepared_eps: Vec<String> = vec![];
        let mut abort_reason: Option<String> = None;

        for (ep, result) in &prepare_results {
            match result {
                Ok(PrepareVote::Prepared) => prepared_eps.push(ep.clone()),
                Ok(PrepareVote::NotChanged) => {} // read-only, skip commit phase
                Ok(PrepareVote::Aborted) => {
                    abort_reason = Some(format!("participant {} voted ABORTED", ep));
                    break;
                }
                Err(e) => {
                    abort_reason = Some(format!("participant {} prepare failed: {}", ep, e));
                    break;
                }
            }
        }

        if let Some(reason) = abort_reason {
            warn!(txn_id, reason, "prepare phase failed — aborting all");
            self.store
                .update_state(txn_id, TransactionState::Aborted)
                .await?;
            Self::do_abort_participants(txn_id, &participants).await;
            self.lease_manager.cancel(&txn.lease_id).await.ok();
            return Err(Error::TransactionAborted(txn_id.to_string()));
        }

        // ── Phase 2: Commit (parallel, PREPARED voters only) ─────────────────
        self.store
            .update_state(txn_id, TransactionState::Prepared)
            .await?;

        let commit_futs: Vec<_> = prepared_eps
            .iter()
            .map(|ep| {
                let ep = ep.clone();
                let tid = txn_id.to_string();
                async move { Self::call_commit(&ep, &tid).await }
            })
            .collect();

        let commit_results = join_all(commit_futs).await;

        // Log commit-phase failures but don't abort — once we passed prepare,
        // we're committed. Participants that failed need recovery (v2 concern).
        for (i, result) in commit_results.iter().enumerate() {
            if let Err(e) = result {
                error!(
                    txn_id,
                    endpoint = %prepared_eps[i],
                    error = %e,
                    "commit-phase call failed — participant needs recovery"
                );
            }
        }

        self.store
            .update_state(txn_id, TransactionState::Committed)
            .await?;
        self.lease_manager.cancel(&txn.lease_id).await.ok();
        debug!(
            txn_id,
            prepared = prepared_eps.len(),
            "transaction committed"
        );
        Ok(())
    }

    // ── Outbound gRPC helpers (static — no self needed) ───────────────────────

    fn connect_to(host_port: &str) -> Result<tonic::transport::Endpoint, Error> {
        let uri = format!("http://{}", host_port);
        tonic::transport::Channel::from_shared(uri)
            .map_err(|e| Error::Internal(format!("invalid endpoint '{}': {}", host_port, e)))
            .map(|ep| {
                ep.connect_timeout(Duration::from_secs(5))
                    .timeout(Duration::from_secs(10))
            })
    }

    async fn call_prepare(endpoint: &str, txn_id: &str) -> Result<PrepareVote, Error> {
        let channel = Self::connect_to(endpoint)?
            .connect()
            .await
            .map_err(|e| Error::Internal(format!("connect to {} failed: {}", endpoint, e)))?;

        let mut client = ParticipantServiceClient::new(channel);
        let resp = client
            .prepare(ParticipantRequest {
                txn_id: txn_id.to_string(),
            })
            .await
            .map_err(|e| Error::Internal(format!("prepare rpc to {} failed: {}", endpoint, e)))?;

        Ok(vote_from_i32(resp.into_inner().vote))
    }

    async fn call_commit(endpoint: &str, txn_id: &str) -> Result<(), Error> {
        let channel = Self::connect_to(endpoint)?
            .connect()
            .await
            .map_err(|e| Error::Internal(format!("connect to {} failed: {}", endpoint, e)))?;

        let mut client = ParticipantServiceClient::new(channel);
        client
            .commit(ParticipantRequest {
                txn_id: txn_id.to_string(),
            })
            .await
            .map_err(|e| Error::Internal(format!("commit rpc to {} failed: {}", endpoint, e)))?;
        Ok(())
    }

    async fn call_abort(endpoint: &str, txn_id: &str) -> Result<(), Error> {
        let channel = Self::connect_to(endpoint)?
            .connect()
            .await
            .map_err(|e| Error::Internal(format!("connect to {} failed: {}", endpoint, e)))?;

        let mut client = ParticipantServiceClient::new(channel);
        client
            .abort(ParticipantRequest {
                txn_id: txn_id.to_string(),
            })
            .await
            .map_err(|e| Error::Internal(format!("abort rpc to {} failed: {}", endpoint, e)))?;
        Ok(())
    }

    async fn call_prepare_and_commit(endpoint: &str, txn_id: &str) -> Result<PrepareVote, Error> {
        let channel = Self::connect_to(endpoint)?
            .connect()
            .await
            .map_err(|e| Error::Internal(format!("connect to {} failed: {}", endpoint, e)))?;

        let mut client = ParticipantServiceClient::new(channel);
        let resp = client
            .prepare_and_commit(ParticipantRequest {
                txn_id: txn_id.to_string(),
            })
            .await
            .map_err(|e| {
                Error::Internal(format!(
                    "prepare_and_commit rpc to {} failed: {}",
                    endpoint, e
                ))
            })?;

        Ok(vote_from_i32(resp.into_inner().vote))
    }

    async fn do_abort_participants(txn_id: &str, participants: &[ParticipantRecord]) {
        let futs: Vec<_> = participants
            .iter()
            .map(|p| {
                let ep = p.endpoint.clone();
                let tid = txn_id.to_string();
                async move {
                    if let Err(e) = Self::call_abort(&ep, &tid).await {
                        warn!(endpoint = %ep, error = %e, "abort call to participant failed");
                    }
                }
            })
            .collect();
        join_all(futs).await;
    }
}

/// Convert proto PrepareVote i32 to core enum.
/// Default to Prepared (0) for unknown values.
fn vote_from_i32(v: i32) -> PrepareVote {
    match v {
        1 => PrepareVote::NotChanged,
        2 => PrepareVote::Aborted,
        _ => PrepareVote::Prepared,
    }
}
