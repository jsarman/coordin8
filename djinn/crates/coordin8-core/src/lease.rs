use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Sentinel: client requests an indefinite lease. Only honored when
/// `LeaseConfig::max_ttl` is `None` (FOREVER); otherwise capped to max.
pub const LEASE_FOREVER: u64 = 0;

/// Sentinel: client defers to the server's preferred TTL.
pub const LEASE_ANY: u64 = u64::MAX;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub lease_id: String,
    pub resource_id: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// The TTL actually granted (seconds). 0 = FOREVER.
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
#[derive(Debug, Clone)]
pub struct LeaseConfig {
    /// `None` = FOREVER allowed (no cap).
    pub max_ttl: Option<u64>,
    /// Returned when a client requests `LEASE_ANY`.
    pub preferred_ttl: u64,
}

impl LeaseConfig {
    pub fn from_env() -> Self {
        let max_ttl = match std::env::var("MAX_LEASE_TTL").ok() {
            Some(v) if v.eq_ignore_ascii_case("forever") => None,
            Some(v) => Some(v.parse::<u64>().unwrap_or(3600)),
            None => Some(3600),
        };
        let preferred_ttl = std::env::var("PREFERRED_LEASE_TTL")
            .ok()
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

/// Abstract lease client used by downstream services (EventMgr, Space, TxnMgr).
///
/// The monolith hands out a local impl that wraps the in-process `LeaseManager`.
/// Split-mode services hand out a remote impl that forwards each call to a
/// `LeaseMgr` instance discovered through Registry. Downstream code is oblivious
/// to the difference — this is the Jini lease-as-interface pattern, adapted for
/// Coordin8's gRPC transport.
///
/// Only the three methods downstream services actually use are on the trait.
/// Internal lease machinery (reaper, `get`, `drain_expired`, `get_by_resource`)
/// stays on the concrete `LeaseManager` — it is implementation detail of the
/// *local* LeaseMgr process, not a cross-service contract.
#[async_trait]
pub trait Leasing: Send + Sync {
    async fn grant(&self, resource_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn renew(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRecord, Error>;
    async fn cancel(&self, lease_id: &str) -> Result<(), Error>;
}
