use std::collections::VecDeque;

use async_trait::async_trait;
use dashmap::DashMap;

use coordin8_core::{Error, EventRecord, EventStore, SubscriptionRecord};

pub struct InMemoryEventStore {
    subscriptions: DashMap<String, SubscriptionRecord>,
    mailboxes: DashMap<String, VecDeque<EventRecord>>,
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
        self.mailboxes.insert(id, VecDeque::new());
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

    async fn enqueue(&self, registration_id: &str, event: EventRecord) -> Result<(), Error> {
        match self.mailboxes.get_mut(registration_id) {
            Some(mut queue) => {
                queue.push_back(event);
                Ok(())
            }
            None => Err(Error::SubscriptionNotFound(registration_id.to_string())),
        }
    }

    async fn dequeue(&self, registration_id: &str) -> Result<Vec<EventRecord>, Error> {
        match self.mailboxes.get_mut(registration_id) {
            Some(mut queue) => Ok(queue.drain(..).collect()),
            None => Err(Error::SubscriptionNotFound(registration_id.to_string())),
        }
    }
}
