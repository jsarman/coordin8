use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::broadcast;
use tracing::debug;
use uuid::Uuid;

use coordin8_core::{
    Error, LeaseRecord, Leasing, SpaceEventKind, SpaceStore, SpaceWatchRecord, TupleRecord,
    TxnEnlister,
};
use coordin8_registry::matcher::{matches, parse_template};

pub struct SpaceManager {
    store: Arc<dyn SpaceStore>,
    lease_manager: Arc<dyn Leasing>,
    tuple_tx: broadcast::Sender<TupleRecord>,
    expiry_tx: broadcast::Sender<TupleRecord>,
    /// Auto-enlist gateway. None = no TxnMgr available; transactional ops still
    /// work locally but the Space is not registered as a 2PC participant. Tests
    /// that drive `commit_space_txn` / `abort_space_txn` directly use this mode.
    enlister: Option<Arc<dyn TxnEnlister>>,
    /// Address advertised to TxnMgr when enlisting — the participant endpoint
    /// the 2PC coordinator dials back during prepare/commit/abort. Required
    /// whenever `enlister` is `Some`.
    participant_endpoint: Option<String>,
    /// Set of transactions this Space has already enlisted under. Prevents
    /// duplicate `Enlist` calls when a single txn does multiple writes/takes.
    /// Cleared on commit/abort.
    enlisted: DashMap<String, ()>,
}

impl SpaceManager {
    /// Construct a SpaceManager without a TxnMgr connection. Transactional
    /// methods (`write` with `txn_id`, etc.) still work but the Space will not
    /// auto-enlist as a 2PC participant — callers are responsible for driving
    /// `commit_space_txn` / `abort_space_txn` themselves.
    pub fn new(
        store: Arc<dyn SpaceStore>,
        lease_manager: Arc<dyn Leasing>,
        tuple_tx: broadcast::Sender<TupleRecord>,
        expiry_tx: broadcast::Sender<TupleRecord>,
    ) -> Self {
        Self {
            store,
            lease_manager,
            tuple_tx,
            expiry_tx,
            enlister: None,
            participant_endpoint: None,
            enlisted: DashMap::new(),
        }
    }

    /// Construct a SpaceManager with auto-enlist wired in. On the first
    /// transactional `write` or `take` for a given `txn_id`, the Space calls
    /// `TxnEnlister::enlist(txn_id, participant_endpoint)` so that the 2PC
    /// coordinator knows to drive prepare/commit/abort against this Space.
    pub fn with_enlister(
        store: Arc<dyn SpaceStore>,
        lease_manager: Arc<dyn Leasing>,
        tuple_tx: broadcast::Sender<TupleRecord>,
        expiry_tx: broadcast::Sender<TupleRecord>,
        enlister: Arc<dyn TxnEnlister>,
        participant_endpoint: String,
    ) -> Self {
        Self {
            store,
            lease_manager,
            tuple_tx,
            expiry_tx,
            enlister: Some(enlister),
            participant_endpoint: Some(participant_endpoint),
            enlisted: DashMap::new(),
        }
    }

    /// Auto-enlist this Space as a participant in `txn_id` if we haven't
    /// already. No-op when no enlister is configured (tests / standalone mode).
    /// Idempotent: subsequent calls for the same txn skip the RPC.
    async fn ensure_enlisted(&self, txn_id: &str) -> Result<(), Error> {
        let Some(enlister) = self.enlister.as_ref() else {
            return Ok(());
        };
        if self.enlisted.contains_key(txn_id) {
            return Ok(());
        }
        let endpoint = self.participant_endpoint.as_deref().ok_or_else(|| {
            Error::Internal("space participant_endpoint not configured".to_string())
        })?;
        enlister.enlist(txn_id, endpoint).await?;
        self.enlisted.insert(txn_id.to_string(), ());
        debug!(txn_id, endpoint, "space auto-enlisted as 2PC participant");
        Ok(())
    }

    /// Write a leased tuple into the Space. Returns both the tuple and its lease.
    /// Jini: JavaSpace.write(Entry, Transaction, long lease)
    ///
    /// When txn_id is Some, the tuple is placed in an uncommitted buffer —
    /// invisible to non-transactional reads, visible only within the same transaction.
    /// On commit, tuples are flushed to the visible store and broadcast.
    /// On abort, tuples are discarded and their leases cancelled.
    pub async fn write(
        &self,
        attrs: HashMap<String, String>,
        payload: Vec<u8>,
        ttl_secs: u64,
        written_by: String,
        input_tuple_id: Option<String>,
        txn_id: Option<String>,
    ) -> Result<(TupleRecord, LeaseRecord), Error> {
        let tuple_id = Uuid::new_v4().to_string();
        let resource_id = format!("space:{}", tuple_id);
        let lease = self.lease_manager.grant(&resource_id, ttl_secs).await?;

        let record = TupleRecord {
            tuple_id: tuple_id.clone(),
            attrs,
            payload,
            lease_id: lease.lease_id.clone(),
            written_by,
            written_at: Utc::now(),
            input_tuple_id,
        };

        if let Some(ref tid) = txn_id {
            // Transactional write — auto-enlist, buffer, don't broadcast yet.
            self.ensure_enlisted(tid).await?;
            self.store.insert_uncommitted(tid, record.clone()).await?;
            debug!(tuple_id, txn_id = tid, lease_id = %lease.lease_id, "tuple written (uncommitted)");
        } else {
            // Non-transactional — insert and broadcast immediately.
            self.store.insert(record.clone()).await?;
            debug!(tuple_id, lease_id = %lease.lease_id, granted_ttl = lease.ttl_seconds, "tuple written");
            let _ = self.tuple_tx.send(record.clone());
        }

        Ok((record, lease))
    }

    /// Non-destructive read by template. Blocking or non-blocking.
    /// Jini: JavaSpace.read() (wait=true) / readIfExists() (wait=false)
    ///
    /// When txn_id is Some, also searches that transaction's uncommitted buffer.
    pub async fn read(
        &self,
        template: HashMap<String, String>,
        wait: bool,
        timeout_ms: u64,
        txn_id: Option<String>,
    ) -> Result<Option<TupleRecord>, Error> {
        // Try immediate match.
        if let Some(record) = self.store.find_match(&template, txn_id.as_deref()).await? {
            return Ok(Some(record));
        }

        if !wait {
            return Ok(None);
        }

        // Block: subscribe to broadcast and wait for a matching tuple.
        let ops = parse_template(&template);
        let mut rx = self.tuple_tx.subscribe();

        let wait_future = async {
            loop {
                match rx.recv().await {
                    Ok(tuple) => {
                        if matches(&ops, &tuple.attrs) {
                            return Some(tuple);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        };

        if timeout_ms > 0 {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), wait_future).await {
                Ok(result) => Ok(result),
                Err(_) => Ok(None),
            }
        } else {
            // timeout_ms == 0 + wait == true → wait indefinitely
            Ok(wait_future.await)
        }
    }

    /// Atomic claim+remove by template. Blocking or non-blocking.
    /// Jini: JavaSpace.take() (wait=true) / takeIfExists() (wait=false)
    ///
    /// When txn_id is Some, searches uncommitted first, then committed.
    /// Takes from committed under a txn are tracked for restore-on-abort.
    pub async fn take(
        &self,
        template: HashMap<String, String>,
        wait: bool,
        timeout_ms: u64,
        txn_id: Option<String>,
    ) -> Result<Option<TupleRecord>, Error> {
        // Auto-enlist before any transactional take (either hits or restores-on-abort).
        if let Some(ref tid) = txn_id {
            self.ensure_enlisted(tid).await?;
        }

        // Try immediate atomic take.
        if let Some(record) = self.store.take_match(&template, txn_id.as_deref()).await? {
            // Only cancel lease immediately for non-transactional takes.
            // Transactional takes defer lease cleanup to commit/abort.
            if txn_id.is_none() {
                let _ = self.lease_manager.cancel(&record.lease_id).await;
            }
            return Ok(Some(record));
        }

        if !wait {
            return Ok(None);
        }

        // Block: subscribe to broadcast, re-query store on each notification.
        let mut rx = self.tuple_tx.subscribe();

        let store = Arc::clone(&self.store);
        let lease_mgr = Arc::clone(&self.lease_manager);
        let template_clone = template.clone();
        let ops = parse_template(&template);
        let txn_id_clone = txn_id.clone();

        let wait_future = async {
            loop {
                match rx.recv().await {
                    Ok(tuple) => {
                        if !matches(&ops, &tuple.attrs) {
                            continue;
                        }
                        // Re-query store atomically — don't trust broadcast alone
                        // because another taker may have claimed it.
                        if let Ok(Some(record)) = store
                            .take_match(&template_clone, txn_id_clone.as_deref())
                            .await
                        {
                            if txn_id_clone.is_none() {
                                let _ = lease_mgr.cancel(&record.lease_id).await;
                            }
                            return Some(record);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // We lagged — try an immediate take in case we missed the signal
                        if let Ok(Some(record)) = store
                            .take_match(&template_clone, txn_id_clone.as_deref())
                            .await
                        {
                            if txn_id_clone.is_none() {
                                let _ = lease_mgr.cancel(&record.lease_id).await;
                            }
                            return Some(record);
                        }
                    }
                }
            }
        };

        if timeout_ms > 0 {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), wait_future).await {
                Ok(result) => Ok(result),
                Err(_) => Ok(None),
            }
        } else {
            Ok(wait_future.await)
        }
    }

    /// Bulk read — returns all tuples matching the template without removing them.
    /// Jini: JavaSpace05.contents()
    pub async fn contents(
        &self,
        template: HashMap<String, String>,
        txn_id: Option<String>,
    ) -> Result<Vec<TupleRecord>, Error> {
        self.store
            .find_all_matches(&template, txn_id.as_deref())
            .await
    }

    /// Commit a transaction's Space operations: flush uncommitted writes to the
    /// visible store and broadcast them. Takes under the txn stay removed.
    pub async fn commit_space_txn(&self, txn_id: &str) -> Result<(), Error> {
        let flushed = self.store.commit_txn(txn_id).await?;
        self.enlisted.remove(txn_id);

        // Broadcast all newly visible tuples to wake blocked readers/takers.
        for record in &flushed {
            let _ = self.tuple_tx.send(record.clone());
        }

        debug!(txn_id, flushed = flushed.len(), "space txn committed");
        Ok(())
    }

    /// Abort a transaction's Space operations: discard uncommitted writes (cancel
    /// their leases) and restore taken tuples back to the visible store.
    /// Restored tuples are broadcast to wake any blocked readers/takers.
    pub async fn abort_space_txn(&self, txn_id: &str) -> Result<(), Error> {
        let (discarded, restored) = self.store.abort_txn(txn_id).await?;
        self.enlisted.remove(txn_id);

        // Cancel leases on discarded uncommitted tuples.
        for record in &discarded {
            let _ = self.lease_manager.cancel(&record.lease_id).await;
        }

        // Broadcast restored tuples — wake blocked read/take waiters.
        for record in &restored {
            let _ = self.tuple_tx.send(record.clone());
        }

        debug!(
            txn_id,
            discarded = discarded.len(),
            restored = restored.len(),
            "space txn aborted"
        );
        Ok(())
    }

    /// Check if this Space has any uncommitted state for the given transaction.
    pub async fn has_txn(&self, txn_id: &str) -> Result<bool, Error> {
        self.store.has_txn(txn_id).await
    }

    /// Create a notification subscription. Returns (watch_id, lease_id).
    /// Jini: JavaSpace.notify()
    pub async fn notify(
        &self,
        template: HashMap<String, String>,
        on: SpaceEventKind,
        ttl_secs: u64,
        handback: Vec<u8>,
    ) -> Result<(String, String), Error> {
        let watch_id = Uuid::new_v4().to_string();
        let resource_id = format!("space-watch:{}", watch_id);
        let lease = self.lease_manager.grant(&resource_id, ttl_secs).await?;

        let record = SpaceWatchRecord {
            watch_id: watch_id.clone(),
            template,
            on,
            lease_id: lease.lease_id.clone(),
            handback,
        };

        self.store.create_watch(record).await?;

        debug!(watch_id, lease_id = %lease.lease_id, "space notification created");

        Ok((watch_id, lease.lease_id))
    }

    /// Subscribe to the tuple appearance broadcast.
    pub fn subscribe_tuple_broadcast(&self) -> broadcast::Receiver<TupleRecord> {
        self.tuple_tx.subscribe()
    }

    /// Subscribe to the tuple expiry broadcast.
    pub fn subscribe_expiry_broadcast(&self) -> broadcast::Receiver<TupleRecord> {
        self.expiry_tx.subscribe()
    }

    /// Called by the lease expiry listener when a space: lease expires.
    pub async fn on_tuple_expired(&self, lease_id: &str) {
        if let Ok(Some(record)) = self.store.remove_by_lease(lease_id).await {
            debug!(tuple_id = %record.tuple_id, lease_id, "tuple expired");
            let _ = self.expiry_tx.send(record);
        }
    }

    /// Called by the lease expiry listener when a space-watch: lease expires.
    pub async fn on_watch_expired(&self, lease_id: &str) {
        if let Ok(Some(watch)) = self.store.remove_watch_by_lease(lease_id).await {
            debug!(watch_id = %watch.watch_id, lease_id, "space notification expired");
        }
    }

    /// Cancel a tuple — remove from Space and cancel its lease.
    pub async fn cancel(&self, tuple_id: &str) -> Result<(), Error> {
        let record = self
            .store
            .remove(tuple_id)
            .await?
            .ok_or_else(|| Error::TupleNotFound(tuple_id.to_string()))?;

        self.lease_manager.cancel(&record.lease_id).await?;

        debug!(tuple_id, "tuple cancelled");
        Ok(())
    }

    /// Renew a tuple's lease.
    pub async fn renew(&self, tuple_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error> {
        let record = self
            .store
            .get(tuple_id)
            .await?
            .ok_or_else(|| Error::TupleNotFound(tuple_id.to_string()))?;

        self.lease_manager.renew(&record.lease_id, ttl_secs).await
    }
}
