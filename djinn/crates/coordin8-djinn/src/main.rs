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
    /// Registry is the well-known anchor and does not self-register.
    Registry,
    /// Boot LeaseMgr alone on COORDIN8_BIND_ADDR.
    ///
    /// If COORDIN8_REGISTRY is set, self-registers under interface=LeaseMgr
    /// with a 30-second self-lease. Otherwise logs a warning and runs
    /// standalone.
    Lease,
    /// Boot EventMgr alone on COORDIN8_BIND_ADDR.
    ///
    /// Requires COORDIN8_REGISTRY to be set — EventMgr discovers LeaseMgr
    /// through Registry (via RemoteLeasing) and self-registers under
    /// interface=EventMgr with a 30-second self-lease.
    Event,
    /// (stub) Split mode not yet implemented for Space.
    Space,
    /// (stub) Split mode not yet implemented for TransactionMgr.
    Txn,
    /// (stub) Split mode not yet implemented for Proxy.
    Proxy,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("coordin8=info".parse()?),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        None | Some(Command::All) => services::run_all().await,
        Some(Command::Registry) => services::run_registry().await,
        Some(Command::Lease) => services::run_lease().await,
        Some(Command::Event) => services::run_event().await,
        Some(Command::Space) => stub_not_implemented("space"),
        Some(Command::Txn) => stub_not_implemented("txn"),
        Some(Command::Proxy) => stub_not_implemented("proxy"),
    }
}

/// Log "split mode not yet implemented" and exit 1.
fn stub_not_implemented(service: &str) -> Result<()> {
    eprintln!("split mode not yet implemented for {service}");
    std::process::exit(1);
}
