use async_trait::async_trait;
use dashmap::DashMap;

use coordin8_core::{Error, SpaceStore, SpaceWatchRecord, TupleRecord};
use coordin8_registry::matcher::{matches, parse_template};

pub struct InMemorySpaceStore {
    tuples: DashMap<String, TupleRecord>,
    lease_index: DashMap<String, String>,       // lease_id → tuple_id
    watches: DashMap<String, SpaceWatchRecord>,
    watch_lease_index: DashMap<String, String>,  // lease_id → watch_id
}

impl InMemorySpaceStore {
    pub fn new() -> Self {
        Self {
            tuples: DashMap::new(),
            lease_index: DashMap::new(),
            watches: DashMap::new(),
            watch_lease_index: DashMap::new(),
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
    ) -> Result<Option<TupleRecord>, Error> {
        let ops = parse_template(template);
        for entry in self.tuples.iter() {
            if matches(&ops, &entry.attrs) {
                return Ok(Some(entry.clone()));
            }
        }
        Ok(None)
    }

    async fn take_match(
        &self,
        template: &std::collections::HashMap<String, String>,
    ) -> Result<Option<TupleRecord>, Error> {
        let ops = parse_template(template);
        // Scan for a match, then atomically remove it.
        // If the remove returns None (raced with another taker), continue scanning.
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
                        return Ok(Some(record));
                    }
                    // Raced — someone else took it, loop and try next match
                    continue;
                }
                None => return Ok(None),
            }
        }
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
