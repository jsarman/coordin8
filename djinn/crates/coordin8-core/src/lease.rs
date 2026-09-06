use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Sentinel: client defers to the server's preferred TTL.
///
/// Deliberately `0` — proto3 gives every unset `uint64` field a zero value,
/// so a client that forgets to set `ttl_seconds` lands here by construction.
/// That accidental default must be the *safe* outcome (a bounded,
/// server-chosen TTL), not an eternal, unreapable lease.
pub const LEASE_ANY: u64 = 0;

/// Sentinel: client requests an indefinite lease. Only honored when
/// `LeaseConfig::max_ttl` is `None` (FOREVER); otherwise capped to max.
///
/// Deliberately `u64::MAX` — implausible as an accidental value, unlike the
/// old `0`, which collided with proto3's default-unset value for `uint64`.
pub const LEASE_FOREVER: u64 = u64::MAX;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub lease_id: String,
    pub resource_id: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// The TTL actually granted (seconds). `u64::MAX` (`LEASE_FOREVER`) = never expires.
    pub ttl_seconds: u64,
}

impl LeaseRecord {
    pub fn is_expired(&self) -> bool {
        // FOREVER leases never expire.
        if self.ttl_seconds == LEASE_FOREVER {
            return false;
        }
        Utc::now() >= self.expires_at
    }
}

/// Server-side lease policy. Read from environment at Djinn startup.
///
/// - `MAX_LEASE_TTL`        — max seconds (or `FOREVER`). Default: 3600.
/// - `PREFERRED_LEASE_TTL`  — seconds returned for `LEASE_ANY`. Default: 300.
/// - `<NAMESPACE>_MAX_LEASE_TTL` / `<NAMESPACE>_PREFERRED_LEASE_TTL` —
///   per-resource-type override (e.g. `SPACE_MAX_LEASE_TTL`,
///   `TXN_MAX_LEASE_TTL`), read by [`LeaseConfig::from_env_for`]. Falls back
///   to the global variables above when unset.
#[derive(Debug, Clone)]
pub struct LeaseConfig {
    /// `None` = FOREVER allowed (no cap).
    pub max_ttl: Option<u64>,
    /// Returned when a client requests `LEASE_ANY`.
    pub preferred_ttl: u64,
}

impl LeaseConfig {
    pub fn from_env() -> Self {
        Self::from_env_for(None)
    }

    /// Read policy for a specific resource namespace (e.g. `"space"`,
    /// `"txn"`), falling back to the global `MAX_LEASE_TTL`/
    /// `PREFERRED_LEASE_TTL` when the namespaced variable isn't set.
    ///
    /// Lets each embedded `LeaseManager` (Registry, Space, EventMgr,
    /// TransactionMgr each have their own) get a policy tuned to its own
    /// resource type — a 2PC transaction and a service registration
    /// shouldn't share one default cap — without requiring every deployment
    /// to set per-namespace env vars if it doesn't care to.
    pub fn from_env_for(namespace: Option<&str>) -> Self {
        let namespaced = |suffix: &str| -> Option<String> {
            let ns = namespace?;
            std::env::var(format!("{}_{}", ns.to_uppercase(), suffix)).ok()
        };

        let max_ttl_raw =
            namespaced("MAX_LEASE_TTL").or_else(|| std::env::var("MAX_LEASE_TTL").ok());
        let max_ttl = match max_ttl_raw {
            Some(v) if v.eq_ignore_ascii_case("forever") => None,
            Some(v) => Some(v.parse::<u64>().unwrap_or(3600)),
            None => Some(3600),
        };

        let preferred_ttl = namespaced("PREFERRED_LEASE_TTL")
            .or_else(|| std::env::var("PREFERRED_LEASE_TTL").ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300);

        Self {
            max_ttl,
            preferred_ttl,
        }
    }

    /// Negotiate the granted TTL from a client-requested value.
    pub fn negotiate(&self, requested: u64) -> u64 {
        match requested {
            LEASE_ANY => self.preferred_ttl,
            LEASE_FOREVER => match self.max_ttl {
                None => LEASE_FOREVER,
                Some(max) => max,
            },
            ttl => match self.max_ttl {
                None => ttl,
                Some(max) => ttl.min(max),
            },
        }
    }
}

impl Default for LeaseConfig {
    fn default() -> Self {
        Self {
            max_ttl: Some(3600),
            preferred_ttl: 300,
        }
    }
}

/// Backing store for lease data. Implemented by providers (local, AWS, etc.).
#[async_trait]
pub trait LeaseStore: Send + Sync {
    async fn create(&self, resource_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn renew(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn cancel(&self, lease_id: &str) -> Result<(), Error>;
    async fn get(&self, lease_id: &str) -> Result<Option<LeaseRecord>, Error>;
    async fn get_by_resource(&self, resource_id: &str) -> Result<Option<LeaseRecord>, Error>;
    /// Returns all records whose expires_at is in the past.
    async fn list_expired(&self) -> Result<Vec<LeaseRecord>, Error>;
    async fn remove(&self, lease_id: &str) -> Result<(), Error>;
}

/// Why a lease was reclaimed — broadcast alongside the record so cascade
/// handlers can react to both the same way (both mean "this resource's
/// backing lease is gone, clean it up") while still telling them apart for
/// logging/observability.
///
/// Matches Jini's `Landlord` distinguishing `cancel` from expiry by promptness
/// and intent, not by whether cleanup happens — cleanup happens either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimReason {
    /// The reaper found the lease past its `expires_at`.
    Expired,
    /// The holder explicitly called `cancel()`.
    Cancelled,
}

/// A lease reclamation event — payload broadcast to cascade handlers and to
/// `WatchExpiry` subscribers, on both natural expiry and explicit cancel.
#[derive(Debug, Clone)]
pub struct LeaseReclaimed {
    pub record: LeaseRecord,
    pub reason: ReclaimReason,
}

/// Lease grantor used by a service that owns leased resources (Registry,
/// Space, EventMgr, TransactionMgr).
///
/// Each service embeds its own `LeaseManager` in-process — matching Jini's
/// `Landlord` pattern, where every service that grants leases implements the
/// grantor itself rather than depending on a shared external service. This
/// trait exists so downstream code (e.g. `coordin8-space`'s auto-enlist path)
/// stays decoupled from the concrete `LeaseManager` type, not to abstract
/// over "local vs. remote" — there is no remote variant anymore.
///
/// Only the three methods downstream services actually use are on the trait.
/// Internal lease machinery (reaper, `get`, `drain_expired`, `get_by_resource`)
/// stays on the concrete `LeaseManager`.
#[async_trait]
pub trait Leasing: Send + Sync {
    async fn grant(&self, resource_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn renew(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn cancel(&self, lease_id: &str) -> Result<(), Error>;
}
