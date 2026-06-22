//! OxidRelay binary entry point.
//!
//! Wires together CLI, configuration and logging. Queue, transports and the
//! HTTP API will be added on top incrementally.

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use oxid_relay_core::Config;

/// OxidRelay - cross-platform mail relay and notification service.
#[derive(Debug, Parser)]
#[command(name = "oxid-relay", version, about)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, default_value = "config.toml", env = "OXID_RELAY_CONFIG")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config = Config::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    oxid_relay_logging::init(&config.logging.level)
        .map_err(|err| anyhow::anyhow!("initialising logging: {err}"))?;

    tracing::info!(
        config = %cli.config.display(),
        queue = %config.queue.database,
        log_level = %config.logging.level,
        "OxidRelay started"
    );

    // TODO: wire queue, transports and the HTTP API.
    Ok(())
}
