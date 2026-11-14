use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use coordin8_auth::ClientAuthConfig;
use coordin8_observability::wrap_traced_channel as wrap_channel;
use futures::future::join_all;
use futures::stream::{self, StreamExt};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use coordin8_core::{EndpointAllowlist, TXN_PARTICIPANT_ALLOW_VAR};
use coordin8_core::{
    Error, LeaseRecord, Leasing, ParticipantRecord, PrepareVote, TransactionRecord,
    TransactionState, TxnStore,
};
use coordin8_proto::coordin8::{
    participant_service_client::ParticipantServiceClient, ParticipantRequest,
};

pub struct TxnManager {
    store: Arc<dyn TxnStore>,
    lease_manager: Arc<dyn Leasing>,
    client_auth: ClientAuthConfig,
    allowlist: EndpointAllowlist,
    /// Transactions whose commit (or recovery) this process is driving right
    /// now. Recovery skips them; after a restart the set is empty, so every
    /// `Voting`/`Prepared` transaction is recovered. Assumes one TxnMgr per
    /// store, as deployed today.
    in_flight: Arc<Mutex<HashSet<String>>>,
    /// Backoff for transactions recovery could not finish (a participant
    /// still unreachable): txn id -> (next attempt, current delay).
    retry_at: Mutex<HashMap<String, (Instant, Duration)>>,
    /// First backoff delay; doubles per failed attempt.
    backoff_base: Duration,
}

/// Removes its txn from `in_flight` when the commit or recovery ends.
struct InFlight {
    set: Arc<Mutex<HashSet<String>>>,
    txn_id: String,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.txn_id);
    }
}

/// How often [`TxnManager::spawn_recovery`] sweeps for stuck transactions.
pub const RECOVERY_INTERVAL: Duration = Duration::from_secs(30);

/// Longest wait between recovery attempts on one transaction whose
/// participant stays unreachable.
const MAX_RECOVERY_BACKOFF: Duration = Duration::from_secs(10 * 60);

/// How many stuck transactions one sweep drives at once, so one dead
/// endpoint (5s connect + 10s call timeout) doesn't hold up the rest.
const RECOVERY_CONCURRENCY: usize = 8;

/// States with a decision, or a vote, still to be delivered.
fn is_unresolved(s: &TransactionState) -> bool {
    matches!(
        s,
        TransactionState::Voting | TransactionState::Prepared | TransactionState::Aborting
    )
}

impl TxnManager {
    pub fn new(store: Arc<dyn TxnStore>, lease_manager: Arc<dyn Leasing>) -> Self {
        Self::with_client_auth(store, lease_manager, ClientAuthConfig::trust())
    }

    /// Same as [`Self::new`], but with an explicit outbound auth strategy
    /// (Decision 8 — `.claude/plans/grpc-security/PRD.md`) for the
    /// `ParticipantService` calls (`Prepare`/`Commit`/`Abort`) this manager
    /// makes against participants (e.g. Space) during 2PC.
    pub fn with_client_auth(
        store: Arc<dyn TxnStore>,
        lease_manager: Arc<dyn Leasing>,
        client_auth: ClientAuthConfig,
    ) -> Self {
        Self {
            store,
            lease_manager,
            client_auth,
            allowlist: EndpointAllowlist::allow_all(TXN_PARTICIPANT_ALLOW_VAR),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            retry_at: Mutex::new(HashMap::new()),
            backoff_base: RECOVERY_INTERVAL,
        }
    }

    /// Restrict which `participant_endpoint`s `enlist` accepts (SSRF /
    /// token-leak guard — see [`EndpointAllowlist`]). Default is allow-all.
    pub fn with_participant_allowlist(mut self, allowlist: EndpointAllowlist) -> Self {
        self.allowlist = allowlist;
        self
    }

    /// First delay before recovery retries a transaction it could not
    /// finish (doubles per attempt, capped). Default [`RECOVERY_INTERVAL`];
    /// tests use zero to retry on every `recover()`.
    pub fn with_recovery_backoff(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
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

        if let Err(e) = self.store.create(record).await {
            // Don't leave a lease with no transaction behind it.
            self.lease_manager.cancel(&lease.lease_id).await.ok();
            return Err(e);
        }
        debug!(txn_id, lease_id = %lease.lease_id, ttl_secs, "transaction begun");
        Ok((txn_id, lease))
    }

    pub async fn enlist(
        &self,
        txn_id: &str,
        endpoint: String,
        crash_count: u64,
    ) -> Result<(), Error> {
        // Well-formedness + allowlist BEFORE anything is stored: a rejected
        // endpoint is never persisted and therefore never dialed.
        self.allowlist.check(&endpoint)?;
        let participant = ParticipantRecord {
            endpoint: endpoint.clone(),
            crash_count,
        };
        // Atomic with the Active check inside the store: an enlist that loses
        // the race against commit()/abort() is rejected with TransactionTerminal.
        self.store.add_participant(txn_id, participant).await?;
        debug!(txn_id, endpoint, crash_count, "participant enlisted");
        Ok(())
    }

    pub async fn get_state(&self, txn_id: &str) -> Result<TransactionState, Error> {
        Ok(self.fetch(txn_id).await?.state)
    }

    async fn fetch(&self, txn_id: &str) -> Result<TransactionRecord, Error> {
        self.store
            .get(txn_id)
            .await?
            .ok_or_else(|| Error::TransactionNotFound(txn_id.to_string()))
    }

    /// Cascade entry point: the lease `lease_id` on resource `txn:<txn_id>`
    /// was reclaimed. Acts only if that is the lease the transaction holds:
    /// `LeaseService.Grant` accepts any resource id, so anyone could grant
    /// and cancel a lease named `txn:<victim>` to abort someone else's
    /// transaction.
    pub async fn on_lease_reclaimed(&self, txn_id: &str, lease_id: &str) -> Result<(), Error> {
        match self.store.get(txn_id).await? {
            Some(t) if t.lease_id == lease_id => self.abort_expired(txn_id).await,
            Some(_) => {
                warn!(
                    txn_id,
                    lease_id, "reclaimed lease is not this transaction's lease — ignoring"
                );
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Abort a transaction due to lease expiry. Does not cancel the lease
    /// (it's already expired). Called by the lease expiry cascade.
    ///
    /// Only an `Active` transaction can be aborted this way. If a commit is in
    /// flight (`Voting`/`Prepared`) it owns the outcome and this is a no-op.
    pub async fn abort_expired(&self, txn_id: &str) -> Result<(), Error> {
        match self
            .store
            .update_state_if(txn_id, TransactionState::Active, TransactionState::Aborting)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                debug!(
                    txn_id,
                    "lease expired but transaction not Active — ignoring"
                );
                return Ok(());
            }
            Err(Error::TransactionNotFound(_)) => return Ok(()), // already cleaned up
            Err(e) => return Err(e),
        }

        // Enlists are rejected from here on, so this list is final.
        if let Some(txn) = self.store.get(txn_id).await? {
            self.deliver_abort(&txn).await?;
        }
        debug!(txn_id, "transaction aborted (lease expired)");
        Ok(())
    }

    pub async fn abort(&self, txn_id: &str) -> Result<(), Error> {
        if !self
            .store
            .update_state_if(txn_id, TransactionState::Active, TransactionState::Aborting)
            .await?
        {
            return match self.fetch(txn_id).await?.state {
                TransactionState::Committed => Err(Error::TransactionTerminal(txn_id.to_string())),
                s if s.is_abort() => Ok(()), // idempotent
                _ => Err(Error::TransactionCommitInProgress(txn_id.to_string())),
            };
        }

        let txn = self.fetch(txn_id).await?;
        self.lease_manager.cancel(&txn.lease_id).await.ok();
        self.deliver_abort(&txn).await?;
        debug!(txn_id, "transaction aborted");
        Ok(())
    }

    pub async fn commit(&self, txn_id: &str) -> Result<(), Error> {
        // Mark in flight before the CAS, so recovery never sees our Voting
        // state as stuck.
        let Some(_guard) = self.claim(txn_id) else {
            return self.settled_or_in_progress(txn_id).await;
        };
        // Claim the commit: only one caller can move Active -> Voting.
        if !self
            .store
            .update_state_if(txn_id, TransactionState::Active, TransactionState::Voting)
            .await?
        {
            let txn = self.fetch(txn_id).await?;
            return match txn.state {
                TransactionState::Committed => Ok(()), // idempotent
                s if s.is_abort() => Err(Error::TransactionAborted(txn_id.to_string())),
                // Nobody in this process is driving it: it's stuck (a failed
                // phase 2, a lost single-participant reply, or a coordinator
                // restart). Finish it now instead of refusing.
                _ => match self.recover_one(&txn).await? {
                    Some(TransactionState::Committed) => Ok(()),
                    Some(_) => Err(Error::TransactionAborted(txn_id.to_string())),
                    // Not every participant has heard yet, but the recorded
                    // state may already be the decision.
                    None => match self.fetch(txn_id).await?.state {
                        TransactionState::Prepared | TransactionState::Committed => Ok(()),
                        s if s.is_abort() => Err(Error::TransactionAborted(txn_id.to_string())),
                        _ => Err(Error::TransactionOutcomeUnknown(txn_id.to_string())),
                    },
                },
            };
        }

        // Re-read after the CAS: enlists are now rejected, so this participant
        // list is final.
        let txn = self.fetch(txn_id).await?;
        let participants = txn.participants.clone();

        // ── Zero participants: trivially committed ────────────────────────────
        if participants.is_empty() {
            self.transition(
                txn_id,
                TransactionState::Voting,
                TransactionState::Committed,
            )
            .await?;
            self.lease_manager.cancel(&txn.lease_id).await.ok();
            debug!(txn_id, "zero-participant commit");
            return Ok(());
        }

        // ── Single-participant optimization: PrepareAndCommit ─────────────────
        if participants.len() == 1 {
            let ep = &participants[0].endpoint;
            match Self::call_prepare_and_commit(ep, txn_id, &self.client_auth).await {
                Ok(PrepareVote::Prepared) | Ok(PrepareVote::NotChanged) => {
                    self.transition(
                        txn_id,
                        TransactionState::Voting,
                        TransactionState::Committed,
                    )
                    .await?;
                    self.lease_manager.cancel(&txn.lease_id).await.ok();
                    debug!(txn_id, "single-participant commit via PrepareAndCommit");
                    return Ok(());
                }
                Ok(PrepareVote::Aborted) => {
                    self.transition(txn_id, TransactionState::Voting, TransactionState::Aborted)
                        .await?;
                    self.lease_manager.cancel(&txn.lease_id).await.ok();
                    warn!(txn_id, endpoint = %ep, "single participant voted ABORTED");
                    return Err(Error::TransactionAborted(txn_id.to_string()));
                }
                Err(Error::Unavailable(e)) => {
                    // Never connected, so the participant never saw the
                    // request: aborting is safe and final.
                    // It still holds the txn's state, though, and must be
                    // told: Aborting until recovery delivers the Abort.
                    warn!(txn_id, endpoint = %ep, error = %e, "participant unreachable — aborting");
                    self.transition(txn_id, TransactionState::Voting, TransactionState::Aborting)
                        .await?;
                    self.lease_manager.cancel(&txn.lease_id).await.ok();
                    return Err(Error::TransactionAborted(txn_id.to_string()));
                }
                Err(e) => {
                    // The request may have reached the participant: it may or
                    // may not have committed. Leave the txn in Voting; recovery
                    // retries PrepareAndCommit (idempotent) to learn the answer.
                    error!(
                        txn_id,
                        endpoint = %ep,
                        error = %e,
                        "PrepareAndCommit failed — commit outcome unknown"
                    );
                    return Err(Error::TransactionOutcomeUnknown(txn_id.to_string()));
                }
            }
        }

        // ── Phase 1: Prepare (parallel) ───────────────────────────────────────
        let prepare_futs: Vec<_> = participants
            .iter()
            .map(|p| {
                let ep = p.endpoint.clone();
                let tid = txn_id.to_string();
                let client_auth = self.client_auth.clone();
                async move {
                    let result = Self::call_prepare(&ep, &tid, &client_auth).await;
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
            self.transition(txn_id, TransactionState::Voting, TransactionState::Aborting)
                .await?;
            self.lease_manager.cancel(&txn.lease_id).await.ok();
            self.deliver_abort(&txn).await?;
            return Err(Error::TransactionAborted(txn_id.to_string()));
        }

        // ── Phase 2: Commit (parallel, PREPARED voters only) ─────────────────
        self.transition(txn_id, TransactionState::Voting, TransactionState::Prepared)
            .await?;

        let commit_futs: Vec<_> = prepared_eps
            .iter()
            .map(|ep| {
                let ep = ep.clone();
                let tid = txn_id.to_string();
                let client_auth = self.client_auth.clone();
                async move { Self::call_commit(&ep, &tid, &client_auth).await }
            })
            .collect();

        let commit_results = join_all(commit_futs).await;

        // Once every participant prepared, the decision is commit; `Prepared`
        // records it. A participant that missed its Commit keeps the txn in
        // `Prepared`, and recovery re-drives Commit until it acks. The caller
        // still gets success: the outcome is decided and durable.
        let mut unacked = 0;
        for (i, result) in commit_results.iter().enumerate() {
            if let Err(e) = result {
                unacked += 1;
                error!(
                    txn_id,
                    endpoint = %prepared_eps[i],
                    error = %e,
                    "commit-phase call failed — recovery will re-drive it"
                );
            }
        }
        if unacked > 0 {
            return Ok(());
        }

        self.transition(
            txn_id,
            TransactionState::Prepared,
            TransactionState::Committed,
        )
        .await?;
        self.lease_manager.cancel(&txn.lease_id).await.ok();
        debug!(
            txn_id,
            prepared = prepared_eps.len(),
            "transaction committed"
        );
        Ok(())
    }

    // ── Recovery ──────────────────────────────────────────────────────────────

    fn claim(&self, txn_id: &str) -> Option<InFlight> {
        self.in_flight
            .lock()
            .unwrap()
            .insert(txn_id.to_string())
            .then(|| InFlight {
                set: Arc::clone(&self.in_flight),
                txn_id: txn_id.to_string(),
            })
    }

    async fn settled_or_in_progress(&self, txn_id: &str) -> Result<(), Error> {
        match self.fetch(txn_id).await?.state {
            TransactionState::Committed => Ok(()),
            s if s.is_abort() => Err(Error::TransactionAborted(txn_id.to_string())),
            _ => Err(Error::TransactionCommitInProgress(txn_id.to_string())),
        }
    }

    /// Finish every unresolved (`Voting`/`Prepared`/`Aborting`) transaction
    /// nobody in this process is driving: the ones a crash, a failed phase-2
    /// or abort call, or a lost single-participant reply left behind. Returns
    /// how many this sweep moved to a final state. Run on boot and on a timer
    /// ([`Self::spawn_recovery`]). A transaction whose participant stays
    /// unreachable is retried with backoff (up to [`MAX_RECOVERY_BACKOFF`]);
    /// the `coordin8_txn_unresolved` gauge counts what is left.
    pub async fn recover(&self) -> Result<usize, Error> {
        let now = Instant::now();
        let all = self.store.list_all().await?;
        let unresolved: Vec<_> = all
            .into_iter()
            .filter(|t| is_unresolved(&t.state))
            .collect();
        let pending = unresolved.len();
        {
            // Forget backoff for transactions that are no longer stuck.
            let ids: HashSet<&str> = unresolved.iter().map(|t| t.txn_id.as_str()).collect();
            self.retry_at
                .lock()
                .unwrap()
                .retain(|id, _| ids.contains(id.as_str()));
        }
        let due: Vec<_> = {
            let retry_at = self.retry_at.lock().unwrap();
            unresolved
                .into_iter()
                .filter(|t| retry_at.get(&t.txn_id).is_none_or(|(at, _)| *at <= now))
                .collect()
        };

        let resolved = stream::iter(due)
            .map(|txn| async move { self.recover_listed(&txn.txn_id).await })
            .buffer_unordered(RECOVERY_CONCURRENCY)
            .filter(|done| std::future::ready(*done))
            .count()
            .await;
        coordin8_observability::set_txn_unresolved(pending.saturating_sub(resolved));
        Ok(resolved)
    }

    /// Recover one listed transaction. True only if this call moved it to a
    /// final state.
    async fn recover_listed(&self, txn_id: &str) -> bool {
        let Some(_guard) = self.claim(txn_id) else {
            return false; // a live commit owns it
        };
        // Re-read under the claim: it may have finished since the list.
        let txn = match self.store.get(txn_id).await {
            Ok(Some(t)) if is_unresolved(&t.state) => t,
            Ok(_) => return false,
            Err(e) => {
                warn!(txn_id, error = %e, "transaction recovery failed");
                return false;
            }
        };
        match self.recover_one(&txn).await {
            Ok(Some(state)) => {
                self.retry_at.lock().unwrap().remove(txn_id);
                info!(txn_id, ?state, "transaction recovered");
                true
            }
            Ok(None) => {
                self.back_off(txn_id);
                false
            }
            Err(e) => {
                self.back_off(txn_id);
                warn!(txn_id, error = %e, "transaction recovery failed");
                false
            }
        }
    }

    fn back_off(&self, txn_id: &str) {
        let mut retry_at = self.retry_at.lock().unwrap();
        let delay = retry_at.get(txn_id).map_or(self.backoff_base, |(_, d)| {
            (*d * 2).min(MAX_RECOVERY_BACKOFF)
        });
        retry_at.insert(txn_id.to_string(), (Instant::now() + delay, delay));
    }

    /// Sweep with [`Self::recover`] now and every `interval`.
    pub fn spawn_recovery(self: &Arc<Self>, interval: Duration) {
        let mgr = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                if let Err(e) = mgr.recover().await {
                    warn!(error = %e, "transaction recovery sweep failed");
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    /// Drive one stuck transaction (caller holds its in-flight claim) as far
    /// as the participants allow. The recorded state is the decision:
    ///
    /// - `Prepared`: every participant voted yes, so the decision is commit.
    ///   Re-send Commit to every participant (a NOTCHANGED voter, or one that
    ///   already committed, must treat it as a no-op) until all ack.
    /// - `Voting` with one participant: the PrepareAndCommit reply was lost.
    ///   Ask again; it is idempotent, and the answer is the outcome.
    /// - `Voting` otherwise: no decision was recorded, so decide abort
    ///   (only if this CAS wins: someone else may have just decided commit).
    /// - `Aborting`: re-send Abort to every participant until all ack.
    ///
    /// `Some(final state)` when finished, `None` when a participant is still
    /// unreachable (the next sweep retries).
    async fn recover_one(
        &self,
        txn: &TransactionRecord,
    ) -> Result<Option<TransactionState>, Error> {
        let id = txn.txn_id.as_str();
        match txn.state {
            TransactionState::Prepared => {
                let acks = join_all(txn.participants.iter().map(|p| {
                    let client_auth = self.client_auth.clone();
                    async move { Self::call_commit(&p.endpoint, id, &client_auth).await }
                }))
                .await;
                if let Some(e) = acks.into_iter().find_map(Result::err) {
                    warn!(txn_id = id, error = %e, "re-driving commit: participant not acked yet");
                    return Ok(None);
                }
                self.finish(txn, TransactionState::Prepared, TransactionState::Committed)
                    .await
            }
            TransactionState::Voting if txn.participants.len() == 1 => {
                let ep = &txn.participants[0].endpoint;
                match Self::call_prepare_and_commit(ep, id, &self.client_auth).await {
                    Ok(PrepareVote::Prepared) | Ok(PrepareVote::NotChanged) => {
                        self.finish(txn, TransactionState::Voting, TransactionState::Committed)
                            .await
                    }
                    Ok(PrepareVote::Aborted) => {
                        self.finish(txn, TransactionState::Voting, TransactionState::Aborted)
                            .await
                    }
                    Err(e) => {
                        warn!(txn_id = id, endpoint = %ep, error = %e, "single participant still unreachable");
                        Ok(None)
                    }
                }
            }
            TransactionState::Voting => {
                if self
                    .finish(txn, TransactionState::Voting, TransactionState::Aborting)
                    .await?
                    .is_none()
                {
                    return Ok(None); // lost the CAS: not ours to abort
                }
                self.deliver_abort(txn).await
            }
            TransactionState::Aborting => self.deliver_abort(txn).await,
            ref other => Ok(Some(other.clone())),
        }
    }

    /// CAS `from -> to` and release the txn lease. `None` if someone else
    /// moved it first (then nothing here is final).
    async fn finish(
        &self,
        txn: &TransactionRecord,
        from: TransactionState,
        to: TransactionState,
    ) -> Result<Option<TransactionState>, Error> {
        if !self
            .store
            .update_state_if(&txn.txn_id, from, to.clone())
            .await?
        {
            return Ok(None);
        }
        self.lease_manager.cancel(&txn.lease_id).await.ok();
        Ok(Some(to))
    }

    /// CAS a state transition owned by the in-flight commit. Failure means
    /// some other actor changed the state under us — an invariant violation.
    async fn transition(
        &self,
        txn_id: &str,
        from: TransactionState,
        to: TransactionState,
    ) -> Result<(), Error> {
        if self
            .store
            .update_state_if(txn_id, from.clone(), to.clone())
            .await?
        {
            Ok(())
        } else {
            error!(
                txn_id,
                ?from,
                ?to,
                "unexpected state during commit transition"
            );
            Err(Error::Internal(format!(
                "transaction {txn_id}: expected state {from:?} during commit"
            )))
        }
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

    async fn call_prepare(
        endpoint: &str,
        txn_id: &str,
        client_auth: &ClientAuthConfig,
    ) -> Result<PrepareVote, Error> {
        let channel = Self::connect_to(endpoint)?
            .connect()
            .await
            .map_err(|e| Error::Unavailable(format!("connect to {} failed: {}", endpoint, e)))?;

        let mut client = ParticipantServiceClient::new(wrap_channel(channel, client_auth));
        let resp = client
            .prepare(ParticipantRequest {
                txn_id: txn_id.to_string(),
            })
            .await
            .map_err(|e| Error::Internal(format!("prepare rpc to {} failed: {}", endpoint, e)))?;

        Ok(vote_from_i32(resp.into_inner().vote))
    }

    async fn call_commit(
        endpoint: &str,
        txn_id: &str,
        client_auth: &ClientAuthConfig,
    ) -> Result<(), Error> {
        let channel = Self::connect_to(endpoint)?
            .connect()
            .await
            .map_err(|e| Error::Unavailable(format!("connect to {} failed: {}", endpoint, e)))?;

        let mut client = ParticipantServiceClient::new(wrap_channel(channel, client_auth));
        client
            .commit(ParticipantRequest {
                txn_id: txn_id.to_string(),
            })
            .await
            .map_err(|e| Error::Internal(format!("commit rpc to {} failed: {}", endpoint, e)))?;
        Ok(())
    }

    async fn call_abort(
        endpoint: &str,
        txn_id: &str,
        client_auth: &ClientAuthConfig,
    ) -> Result<(), Error> {
        let channel = Self::connect_to(endpoint)?
            .connect()
            .await
            .map_err(|e| Error::Unavailable(format!("connect to {} failed: {}", endpoint, e)))?;

        let mut client = ParticipantServiceClient::new(wrap_channel(channel, client_auth));
        client
            .abort(ParticipantRequest {
                txn_id: txn_id.to_string(),
            })
            .await
            .map_err(|e| Error::Internal(format!("abort rpc to {} failed: {}", endpoint, e)))?;
        Ok(())
    }

    async fn call_prepare_and_commit(
        endpoint: &str,
        txn_id: &str,
        client_auth: &ClientAuthConfig,
    ) -> Result<PrepareVote, Error> {
        let channel = Self::connect_to(endpoint)?
            .connect()
            .await
            .map_err(|e| Error::Unavailable(format!("connect to {} failed: {}", endpoint, e)))?;

        let mut client = ParticipantServiceClient::new(wrap_channel(channel, client_auth));
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

    /// Send Abort to every participant of a transaction in `Aborting`, in
    /// parallel. When all ack, move it to `Aborted`; otherwise it stays
    /// `Aborting` and recovery re-sends. `Some(Aborted)` when finished.
    async fn deliver_abort(
        &self,
        txn: &TransactionRecord,
    ) -> Result<Option<TransactionState>, Error> {
        let id = txn.txn_id.as_str();
        let acks = join_all(txn.participants.iter().map(|p| {
            let client_auth = self.client_auth.clone();
            async move {
                let r = Self::call_abort(&p.endpoint, id, &client_auth).await;
                if let Err(e) = &r {
                    warn!(txn_id = id, endpoint = %p.endpoint, error = %e, "abort not acked yet — recovery will re-send");
                }
                r
            }
        }))
        .await;
        if acks.iter().any(Result::is_err) {
            return Ok(None);
        }
        self.finish(txn, TransactionState::Aborting, TransactionState::Aborted)
            .await
    }
}

/// Convert proto PrepareVote i32 to core enum.
/// Fail safe: only explicitly recognized values map to a committable vote.
/// Unknown values (garbled wire data, newer proto enum variants) map to
/// Aborted — never default toward commit.
/// Decode a wire vote. Only an explicit PREPARED or NOTCHANGED counts; 0
/// (`VOTE_UNSPECIFIED`, what an empty response decodes to) and any unknown
/// value are ABORTED, so nothing ever defaults toward commit.
fn vote_from_i32(v: i32) -> PrepareVote {
    use coordin8_proto::coordin8::PrepareVote as Wire;
    match Wire::try_from(v) {
        Ok(Wire::VotePrepared) => PrepareVote::Prepared,
        Ok(Wire::VoteNotchanged) => PrepareVote::NotChanged,
        _ => PrepareVote::Aborted,
    }
}

/// In-process [`TxnEnlister`] that calls directly into the bundled `TxnManager`.
///
/// Used in bundled-mode Djinn so the Space (and any other future participant)
/// can auto-enlist without going through gRPC. The split-mode counterpart lives
/// in `coordin8-bootstrap` as `RemoteTxnEnlister`.
pub struct LocalTxnEnlister {
    manager: Arc<TxnManager>,
}

impl LocalTxnEnlister {
    pub fn new(manager: Arc<TxnManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl coordin8_core::TxnEnlister for LocalTxnEnlister {
    async fn enlist(&self, txn_id: &str, endpoint: &str) -> Result<(), Error> {
        self.manager.enlist(txn_id, endpoint.to_string(), 0).await
    }

    async fn state(&self, txn_id: &str) -> Result<Option<TransactionState>, Error> {
        match self.manager.get_state(txn_id).await {
            Ok(s) => Ok(Some(s)),
            Err(Error::TransactionNotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
