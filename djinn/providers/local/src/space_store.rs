use async_trait::async_trait;
use dashmap::DashMap;

use coordin8_core::{Error, SpaceStore, SpaceWatchRecord, TupleRecord};
use coordin8_registry::matcher::{matches, parse_template};

pub struct InMemorySpaceStore {
    tuples: DashMap<String, TupleRecord>,
    lease_index: DashMap<String, String>,       // lease_id → tuple_id
    watches: DashMap<String, SpaceWatchRecord>,
    watch_lease_index: DashMap<String, String>,  // lease_id → watch_id

    // Transaction isolation buffers
    uncommitted: DashMap<String, Vec<TupleRecord>>,   // txn_id → written tuples (not yet visible)
    txn_taken: DashMap<String, Vec<TupleRecord>>,     // txn_id → tuples taken from committed (restore on abort)
}

impl InMemorySpaceStore {
    pub fn new() -> Self {
        Self {
            tuples: DashMap::new(),
            lease_index: DashMap::new(),
            watches: DashMap::new(),
            watch_lease_index: DashMap::new(),
            uncommitted: DashMap::new(),
            txn_taken: DashMap::new(),
        }
    }
}

impl Default for InMemorySpaceStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SpaceStore for InMemorySpaceStore {
    async fn insert(&self, record: TupleRecord) -> Result<(), Error> {
        let tuple_id = record.tuple_id.clone();
        let lease_id = record.lease_id.clone();
        self.tuples.insert(tuple_id.clone(), record);
        self.lease_index.insert(lease_id, tuple_id);
        Ok(())
    }

    async fn insert_uncommitted(&self, txn_id: &str, record: TupleRecord) -> Result<(), Error> {
        self.uncommitted
            .entry(txn_id.to_string())
            .or_default()
            .push(record);
        Ok(())
    }

    async fn get(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error> {
        Ok(self.tuples.get(tuple_id).map(|r| r.clone()))
    }

    async fn remove(&self, tuple_id: &str) -> Result<Option<TupleRecord>, Error> {
        if let Some((_, record)) = self.tuples.remove(tuple_id) {
            self.lease_index.remove(&record.lease_id);
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<TupleRecord>, Error> {
        if let Some((_, tuple_id)) = self.lease_index.remove(lease_id) {
            if let Some((_, record)) = self.tuples.remove(&tuple_id) {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    async fn find_match(
        &self,
        template: &std::collections::HashMap<String, String>,
        txn_id: Option<&str>,
    ) -> Result<Option<TupleRecord>, Error> {
        let ops = parse_template(template);

        // Search committed tuples first.
        for entry in self.tuples.iter() {
            if matches(&ops, &entry.attrs) {
                return Ok(Some(entry.clone()));
            }
        }

        // If txn_id provided, also search that transaction's uncommitted buffer.
        if let Some(tid) = txn_id {
            if let Some(buf) = self.uncommitted.get(tid) {
                for record in buf.iter() {
                    if matches(&ops, &record.attrs) {
                        return Ok(Some(record.clone()));
                    }
                }
            }
        }

        Ok(None)
    }

    async fn take_match(
        &self,
        template: &std::collections::HashMap<String, String>,
        txn_id: Option<&str>,
    ) -> Result<Option<TupleRecord>, Error> {
        let ops = parse_template(template);

        // If txn_id provided, search uncommitted buffer first.
        if let Some(tid) = txn_id {
            if let Some(mut buf) = self.uncommitted.get_mut(tid) {
                if let Some(pos) = buf.iter().position(|r| matches(&ops, &r.attrs)) {
                    let record = buf.remove(pos);
                    return Ok(Some(record));
                }
            }
        }

        // Search committed tuples.
        loop {
            let candidate = self.tuples.iter().find_map(|entry| {
                if matches(&ops, &entry.attrs) {
                    Some(entry.key().clone())
                } else {
                    None
                }
            });

            match candidate {
                Some(tuple_id) => {
                    if let Some((_, record)) = self.tuples.remove(&tuple_id) {
                        self.lease_index.remove(&record.lease_id);

                        // Under a transaction, track the taken tuple for restore-on-abort.
                        if let Some(tid) = txn_id {
                            self.txn_taken
                                .entry(tid.to_string())
                                .or_default()
                                .push(record.clone());
                        }

                        return Ok(Some(record));
                    }
                    // Raced — someone else took it, loop and try next match
                    continue;
                }
                None => return Ok(None),
            }
        }
    }

    async fn find_all_matches(
        &self,
        template: &std::collections::HashMap<String, String>,
        txn_id: Option<&str>,
    ) -> Result<Vec<TupleRecord>, Error> {
        let ops = parse_template(template);
        let mut results = Vec::new();

        // Committed tuples.
        for entry in self.tuples.iter() {
            if ops.is_empty() || matches(&ops, &entry.attrs) {
                results.push(entry.clone());
            }
        }

        // Uncommitted tuples for this transaction.
        if let Some(tid) = txn_id {
            if let Some(buf) = self.uncommitted.get(tid) {
                for record in buf.iter() {
                    if ops.is_empty() || matches(&ops, &record.attrs) {
                        results.push(record.clone());
                    }
                }
            }
        }

        Ok(results)
    }

    async fn commit_txn(&self, txn_id: &str) -> Result<Vec<TupleRecord>, Error> {
        // Flush uncommitted writes to the visible store.
        let flushed = if let Some((_, tuples)) = self.uncommitted.remove(txn_id) {
            for record in &tuples {
                let tuple_id = record.tuple_id.clone();
                let lease_id = record.lease_id.clone();
                self.tuples.insert(tuple_id.clone(), record.clone());
                self.lease_index.insert(lease_id, tuple_id);
            }
            tuples
        } else {
            vec![]
        };

        // Finalize takes — they stay removed, just clean up the tracking buffer.
        self.txn_taken.remove(txn_id);

        Ok(flushed)
    }

    async fn abort_txn(&self, txn_id: &str) -> Result<(Vec<TupleRecord>, Vec<TupleRecord>), Error> {
        // Discard uncommitted writes — return them for lease cleanup.
        let discarded = if let Some((_, tuples)) = self.uncommitted.remove(txn_id) {
            tuples
        } else {
            vec![]
        };

        // Restore taken tuples back to the committed store.
        let restored = if let Some((_, taken)) = self.txn_taken.remove(txn_id) {
            for record in &taken {
                let tuple_id = record.tuple_id.clone();
                let lease_id = record.lease_id.clone();
                self.tuples.insert(tuple_id.clone(), record.clone());
                self.lease_index.insert(lease_id, tuple_id);
            }
            taken
        } else {
            vec![]
        };

        Ok((discarded, restored))
    }

    async fn has_txn(&self, txn_id: &str) -> Result<bool, Error> {
        let has_writes = self.uncommitted.get(txn_id).map_or(false, |v| !v.is_empty());
        let has_takes = self.txn_taken.contains_key(txn_id);
        Ok(has_writes || has_takes)
    }

    async fn list_all(&self) -> Result<Vec<TupleRecord>, Error> {
        Ok(self.tuples.iter().map(|r| r.clone()).collect())
    }

    async fn create_watch(&self, watch: SpaceWatchRecord) -> Result<(), Error> {
        let watch_id = watch.watch_id.clone();
        let lease_id = watch.lease_id.clone();
        self.watches.insert(watch_id.clone(), watch);
        self.watch_lease_index.insert(lease_id, watch_id);
        Ok(())
    }

    async fn remove_watch(&self, watch_id: &str) -> Result<(), Error> {
        if let Some((_, watch)) = self.watches.remove(watch_id) {
            self.watch_lease_index.remove(&watch.lease_id);
        }
        Ok(())
    }

    async fn remove_watch_by_lease(
        &self,
        lease_id: &str,
    ) -> Result<Option<SpaceWatchRecord>, Error> {
        if let Some((_, watch_id)) = self.watch_lease_index.remove(lease_id) {
            if let Some((_, watch)) = self.watches.remove(&watch_id) {
                return Ok(Some(watch));
            }
        }
        Ok(None)
    }

    async fn list_watches(&self) -> Result<Vec<SpaceWatchRecord>, Error> {
        Ok(self.watches.iter().map(|r| r.clone()).collect())
    }
}
