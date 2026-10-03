use std::collections::BTreeMap;

use async_trait::async_trait;
use dashmap::DashMap;

use coordin8_core::{Error, EventRecord, EventStore, SubscriptionRecord};

pub struct InMemoryEventStore {
    subscriptions: DashMap<String, SubscriptionRecord>,
    mailboxes: DashMap<String, Mailbox>,
}

/// Per-registration mailbox: entries keyed by a monotonic per-registration
/// sequence. `last_seq` only grows, so seqs are never reused after an ack.
#[derive(Default)]
struct Mailbox {
    last_seq: u64,
    entries: BTreeMap<u64, EventRecord>,
}

impl InMemoryEventStore {
    pub fn new() -> Self {
        Self {
            subscriptions: DashMap::new(),
            mailboxes: DashMap::new(),
        }
    }
}

impl Default for InMemoryEventStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventStore for InMemoryEventStore {
    async fn create_subscription(&self, sub: SubscriptionRecord) -> Result<(), Error> {
        let id = sub.registration_id.clone();
        self.subscriptions.insert(id.clone(), sub);
        self.mailboxes.insert(id, Mailbox::default());
        Ok(())
    }

    async fn get_subscription(
        &self,
        registration_id: &str,
    ) -> Result<Option<SubscriptionRecord>, Error> {
        Ok(self.subscriptions.get(registration_id).map(|r| r.clone()))
    }

    async fn remove_subscription(&self, registration_id: &str) -> Result<(), Error> {
        self.subscriptions.remove(registration_id);
        self.mailboxes.remove(registration_id);
        Ok(())
    }

    async fn remove_by_lease(&self, lease_id: &str) -> Result<Option<SubscriptionRecord>, Error> {
        // Find the subscription with this lease_id
        let found = self
            .subscriptions
            .iter()
            .find(|r| r.value().lease_id == lease_id)
            .map(|r| r.key().clone());

        if let Some(reg_id) = found {
            let removed = self.subscriptions.remove(&reg_id).map(|(_, v)| v);
            self.mailboxes.remove(&reg_id);
            Ok(removed)
        } else {
            Ok(None)
        }
    }

    async fn list_subscriptions(&self) -> Result<Vec<SubscriptionRecord>, Error> {
        Ok(self.subscriptions.iter().map(|r| r.clone()).collect())
    }

    async fn enqueue(&self, registration_id: &str, mut event: EventRecord) -> Result<u64, Error> {
        match self.mailboxes.get_mut(registration_id) {
            Some(mut mailbox) => {
                mailbox.last_seq += 1;
                let seq = mailbox.last_seq;
                event.seq_num = seq;
                mailbox.entries.insert(seq, event);
                Ok(seq)
            }
            None => Err(Error::SubscriptionNotFound(registration_id.to_string())),
        }
    }

    async fn peek(
        &self,
        registration_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<EventRecord>, Error> {
        Ok(match self.mailboxes.get(registration_id) {
            Some(mailbox) => mailbox
                .entries
                .range((
                    std::ops::Bound::Excluded(after_seq),
                    std::ops::Bound::Unbounded,
                ))
                .take(limit)
                .map(|(_, e)| e.clone())
                .collect(),
            None => vec![],
        })
    }

    async fn ack(&self, registration_id: &str, seq_nums: &[u64]) -> Result<(), Error> {
        if let Some(mut mailbox) = self.mailboxes.get_mut(registration_id) {
            for seq in seq_nums {
                mailbox.entries.remove(seq);
            }
        }
        Ok(())
    }
}
