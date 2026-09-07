use anyhow::Result;
use clap::{Parser, Subcommand};

use coordin8_djinn::services;

// ── CLI ───────────────────────────────────────────────────────────────────────

/// Coordin8 Djinn — distributed coordination runtime.
///
/// With no subcommand (or `all`), boots the full monolith on fixed ports.
/// Subcommands boot individual services in split mode.
#[derive(Parser, Debug)]
#[command(name = "djinn", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Boot all services (same as no subcommand — the monolith).
    All,
    /// Boot the Registry service alone on COORDIN8_BIND_ADDR.
    ///
    /// Registry is the well-known anchor and does not self-register. Embeds
    /// its own LeaseManager for its own entries' leases — no external
    /// leasing dependency at all.
    Registry,
    /// Boot EventMgr alone on COORDIN8_BIND_ADDR.
    ///
    /// Requires COORDIN8_REGISTRY to be set for self-registration under
    /// interface=EventMgr with a 30-second self-lease. Embeds its own
    /// LeaseManager for subscription leases.
    Event,
    /// Boot Space alone on COORDIN8_BIND_ADDR.
    ///
    /// Requires COORDIN8_REGISTRY to be set for self-registration under
    /// interface=Space with a 30-second self-lease, and for lazy TxnMgr
    /// discovery (auto-enlist). Embeds its own LeaseManager for tuple and
    /// watch leases. Mounts both the SpaceService and its 2PC
    /// ParticipantService on the same port.
    Space,
    /// Boot TransactionMgr alone on COORDIN8_BIND_ADDR.
    ///
    /// Requires COORDIN8_REGISTRY to be set for self-registration under
    /// interface=TransactionMgr with a 30-second self-lease. Embeds its own
    /// LeaseManager for transaction leases.
    Txn,
    /// Boot Proxy alone on COORDIN8_BIND_ADDR.
    ///
    /// Requires COORDIN8_REGISTRY to be set — Proxy resolves templates via
    /// Registry's Lookup RPC (RemoteCapabilityResolver) and self-registers
    /// under interface=Proxy with a 30-second self-lease. Reads the usual
    /// PROXY_BIND_HOST / PROXY_PORT_MIN / PROXY_PORT_MAX env vars.
    Proxy,
    /// Check a running Djinn service's health via the standard gRPC Health
    /// Checking Protocol. Exits 0 if SERVING, 1 otherwise (NOT_SERVING,
    /// unreachable, or timed out). Meant for `docker healthcheck` / `docker
    /// compose` — no separate grpc_health_probe binary needed.
    Healthcheck {
        /// gRPC target to check, e.g. `http://127.0.0.1:9002`.
        #[arg(long)]
        addr: String,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Skip the log subscriber for healthcheck — it runs every few seconds
    // under `docker healthcheck`, and its own pass/fail is signaled purely
    // via exit code, not logs.
    let service_name = match &cli.command {
        None | Some(Command::All) => Some("coordin8-djinn"),
        Some(Command::Registry) => Some("coordin8-registry"),
        Some(Command::Event) => Some("coordin8-event"),
        Some(Command::Space) => Some("coordin8-space"),
        Some(Command::Txn) => Some("coordin8-txn"),
        Some(Command::Proxy) => Some("coordin8-proxy"),
        Some(Command::Healthcheck { .. }) => None,
    };
    if let Some(service_name) = service_name {
        coordin8_observability::init(service_name);
        // Fire-and-forget, same as every self-registration task elsewhere
        // in this binary — dropping the JoinHandle detaches, it does not
        // abort. A no-op (binds nothing) unless COORDIN8_METRICS_PORT is set.
        coordin8_observability::MetricsConfig::from_env().spawn();
    }

    match cli.command {
        None | Some(Command::All) => services::run_all().await,
        Some(Command::Registry) => services::run_registry().await,
        Some(Command::Event) => services::run_event().await,
        Some(Command::Space) => services::run_space().await,
        Some(Command::Txn) => services::run_txn().await,
        Some(Command::Proxy) => services::run_proxy().await,
        Some(Command::Healthcheck { addr }) => services::run_healthcheck(&addr).await,
    }
}
