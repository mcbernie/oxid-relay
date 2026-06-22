//! OxidRelay binary entry point.
//!
//! Wires together CLI, configuration and logging. Queue, transports and the
//! HTTP API will be added on top incrementally.

use std::path::PathBuf;

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use oxid_relay_core::Config;
use oxid_relay_plugin::{ReqwestClient, build_engine, discover, plugin_dirs};

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

    // Plugin discovery builds a blocking HTTP client and compiles scripts;
    // run it off the async runtime.
    if let Err(err) = tokio::task::spawn_blocking(discover_plugins).await {
        tracing::warn!(error = %err, "plugin scan task failed");
    }

    // TODO: wire queue, transports and the dispatcher.
    Ok(())
}

/// Scans the platform plugin directories and logs the discovered plugins.
fn discover_plugins() {
    let http = match ReqwestClient::new() {
        Ok(client) => Arc::new(client),
        Err(err) => {
            tracing::warn!(error = %err, "could not build HTTP client, skipping plugin scan");
            return;
        }
    };
    let engine = build_engine(http);

    for dir in plugin_dirs() {
        match discover(&engine, &dir) {
            Ok(plugins) => {
                for plugin in &plugins {
                    tracing::info!(
                        name = %plugin.manifest.name,
                        version = %plugin.manifest.version,
                        kind = %plugin.manifest.kind,
                        dir = %plugin.dir.display(),
                        "plugin loaded"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "plugin discovery failed");
            }
        }
    }
}
