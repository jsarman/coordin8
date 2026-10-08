//! Orphan sweep — the safety net under the lease-reclaim cascade (#60).
//!
//! The cascade (`spawn_cascade`) cleans up a resource when its lease is
//! reclaimed, but only if it *sees* the reclaim: delivery is an in-process
//! broadcast, so it is at-most-once. On a durable provider a reclaim is lost
//! when the lease lapses while the Djinn is down (native TTL can delete the
//! row before the reaper ever lists it), when the cascade lags, or when the
//! process dies between removing the lease and broadcasting. The resource —
//! a Registry entry, tuple, watch, subscription — then outlives its lease
//! forever, and lookups keep returning it.
//!
//! The sweep periodically lists a service's lease-held resources and
//! reclaims any whose lease no longer exists, through the same cleanup the
//! cascade uses. Absence is the signal: an *expired* lease that still exists
//! is the reaper's to reclaim, not ours.
//!
//! A resource is reclaimed only after it is seen without a lease on two
//! consecutive passes. One miss isn't proof: a resource can be written a
//! moment before its lease is readable (and DynamoDB reads are eventually
//! consistent by default), and deleting a live entry is far worse than
//! keeping a dead one a little longer.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use coordin8_core::Error;
use coordin8_lease::LeaseManager;

/// One lease-held resource: its lease, and what the reclaim needs to find it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Held {
    pub lease_id: String,
    /// The resource's own id (e.g. a txn id), for reclaims keyed by it.
    pub id: String,
    /// Which kind of resource, for services holding several (tuple | watch).
    pub kind: &'static str,
}

impl Held {
    pub fn new(kind: &'static str, id: impl Into<String>, lease_id: impl Into<String>) -> Self {
        Self {
            lease_id: lease_id.into(),
            id: id.into(),
            kind,
        }
    }
}

/// First pass shortly after boot (a long outage leaves orphans to clear),
/// then every `SWEEP_EVERY`.
pub const FIRST_SWEEP_AFTER: Duration = Duration::from_secs(10);
pub const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// Two-strike bookkeeping: what was leaseless last pass.
#[derive(Default)]
pub struct OrphanTracker {
    suspects: HashSet<Held>,
}

impl OrphanTracker {
    /// Record this pass's leaseless resources; return those also leaseless
    /// on the previous pass (to reclaim now). The rest become suspects.
    pub fn pass(&mut self, leaseless: HashSet<Held>) -> Vec<Held> {
        let (confirmed, fresh): (Vec<Held>, Vec<Held>) = leaseless
            .into_iter()
            .partition(|h| self.suspects.contains(h));
        self.suspects = fresh.into_iter().collect();
        confirmed
    }
}

/// One pass: list, find the leaseless, reclaim the confirmed. Returns how
/// many were reclaimed. A lease lookup that errors counts as present: a
/// storage hiccup must never look like absence.
pub async fn sweep_once<L, LF, R, RF>(
    leases: &LeaseManager,
    tracker: &mut OrphanTracker,
    list: &L,
    reclaim: &R,
) -> Result<usize, Error>
where
    L: Fn() -> LF,
    LF: Future<Output = Result<Vec<Held>, Error>>,
    R: Fn(Held) -> RF,
    RF: Future<Output = ()>,
{
    let mut leaseless = HashSet::new();
    for held in list().await? {
        if let Ok(None) = leases.get(&held.lease_id).await {
            leaseless.insert(held);
        }
    }
    let confirmed = tracker.pass(leaseless);
    let n = confirmed.len();
    for held in confirmed {
        reclaim(held).await;
    }
    Ok(n)
}

/// Run the sweep for one service for the life of the process.
pub fn spawn_orphan_sweep<L, LF, R, RF>(
    label: &'static str,
    leases: Arc<LeaseManager>,
    list: L,
    reclaim: R,
) where
    L: Fn() -> LF + Send + Sync + 'static,
    LF: Future<Output = Result<Vec<Held>, Error>> + Send,
    R: Fn(Held) -> RF + Send + Sync + 'static,
    RF: Future<Output = ()> + Send,
{
    tokio::spawn(async move {
        let mut tracker = OrphanTracker::default();
        tokio::time::sleep(FIRST_SWEEP_AFTER).await;
        let mut ticker = tokio::time::interval(SWEEP_EVERY);
        loop {
            ticker.tick().await; // the first tick is immediate
            match sweep_once(&leases, &mut tracker, &list, &reclaim).await {
                Ok(0) => {}
                Ok(n) => tracing::warn!(
                    "{label}: reclaimed {n} orphan(s) whose lease was gone \
                     (a lost reclaim — e.g. the lease lapsed while the Djinn was down)"
                ),
                Err(e) => tracing::warn!("{label}: orphan sweep failed: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use coordin8_core::{LeaseConfig, LeaseStore, Leasing};
    use coordin8_provider_local::InMemoryLeaseStore;
    use std::sync::Mutex;
    use tokio::sync::broadcast;

    fn h(id: &str, lease: &str) -> Held {
        Held::new("entry", id, lease)
    }

    #[test]
    fn two_strikes_before_reclaim() {
        let mut t = OrphanTracker::default();
        let a = h("a", "la");
        let b = h("b", "lb");
        assert!(
            t.pass([a.clone()].into()).is_empty(),
            "one miss is not proof"
        );
        assert_eq!(t.pass([a.clone(), b.clone()].into()), vec![a.clone()]);
        // b reappeared with its lease: no longer a suspect.
        assert!(t.pass(HashSet::new()).is_empty());
        assert!(t.pass([b.clone()].into()).is_empty(), "the count restarts");
    }

    #[tokio::test]
    async fn reclaims_only_resources_whose_lease_is_gone() {
        let store: Arc<dyn LeaseStore> = Arc::new(InMemoryLeaseStore::new());
        let (tx, _) = broadcast::channel(16);
        let leases = LeaseManager::new(Arc::clone(&store), LeaseConfig::default(), tx);
        let live = leases.grant("live", 60).await.unwrap();
        let gone = leases.grant("gone", 60).await.unwrap();
        // The reclaim was lost: the lease row vanished, the resource stayed.
        store.cancel(&gone.lease_id).await.unwrap();

        let held = vec![h("live", &live.lease_id), h("gone", &gone.lease_id)];
        let reclaimed = Mutex::new(Vec::new());
        let list = || {
            let held = held.clone();
            async move { Ok(held) }
        };
        let reclaim = |x: Held| {
            reclaimed.lock().unwrap().push(x.id.clone());
            async {}
        };
        let mut t = OrphanTracker::default();
        assert_eq!(
            sweep_once(&leases, &mut t, &list, &reclaim).await.unwrap(),
            0
        );
        assert_eq!(
            sweep_once(&leases, &mut t, &list, &reclaim).await.unwrap(),
            1
        );
        assert_eq!(*reclaimed.lock().unwrap(), vec!["gone".to_string()]);
    }
}
