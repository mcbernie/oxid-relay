//! OxidRelay binary entry point.
//!
//! Wires together CLI, configuration, logging, the queue, the transports
//! (SMTP plus Rhai plugins) and the background dispatcher.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use oxid_relay_core::{Config, Transport};
use oxid_relay_dispatcher::{Dispatcher, DispatcherConfig};
use oxid_relay_plugin::{
    ReqwestClient, RhaiTransport, build_engine, discover, plugin_dirs, string_config_map,
};
use oxid_relay_queue_sqlite::SqliteQueue;
use oxid_relay_transport_smtp::SmtpTransport;

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

    // Durable queue.
    let db_url = sqlite_url(&config.queue.database);
    let queue = Arc::new(
        SqliteQueue::connect(&db_url)
            .await
            .with_context(|| format!("opening queue {db_url}"))?,
    );

    // Available transports keyed by name.
    let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
    let mut default_transport = None;

    if let Some(smtp) = &config.mail.smtp {
        match SmtpTransport::new(smtp) {
            Ok(transport) => {
                transports.insert("smtp".to_string(), Arc::new(transport));
                default_transport = Some("smtp".to_string());
                tracing::info!(host = %smtp.host, "smtp transport ready");
            }
            Err(err) => tracing::warn!(error = %err, "smtp transport unavailable"),
        }
    }

    for (name, transport) in build_plugin_transports(&config) {
        tracing::info!(plugin = %name, "plugin transport ready");
        transports.insert(name, transport);
    }

    if transports.is_empty() {
        tracing::warn!("no transports configured; mails cannot be delivered");
    }

    // Background dispatcher; runs until Ctrl-C.
    let dispatcher = Dispatcher::new(
        queue,
        transports,
        default_transport,
        DispatcherConfig::default(),
    );
    tracing::info!("dispatcher running, press Ctrl-C to stop");
    dispatcher
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;

    tracing::info!("OxidRelay stopped");
    Ok(())
}

/// Builds a SQLite connection URL from the configured database value.
fn sqlite_url(database: &str) -> String {
    if database.starts_with("sqlite:") {
        database.to_string()
    } else {
        format!("sqlite:{database}")
    }
}

/// Discovers plugins and turns each into a transport, resolving its settings
/// (including `_env` secrets) from the configuration.
fn build_plugin_transports(config: &Config) -> Vec<(String, Arc<dyn Transport>)> {
    let http = match ReqwestClient::new() {
        Ok(client) => Arc::new(client),
        Err(err) => {
            tracing::warn!(error = %err, "could not build HTTP client, skipping plugins");
            return Vec::new();
        }
    };
    let engine = Arc::new(build_engine(http));

    let mut transports = Vec::new();
    for dir in plugin_dirs() {
        let plugins = match discover(engine.as_ref(), &dir) {
            Ok(plugins) => plugins,
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "plugin discovery failed");
                continue;
            }
        };
        for plugin in plugins {
            let name = plugin.manifest.name.clone();
            let settings = match config.plugin_settings(&name) {
                Ok(settings) => settings,
                Err(err) => {
                    tracing::warn!(plugin = %name, error = %err, "plugin config unresolved, skipping");
                    continue;
                }
            };
            let transport = RhaiTransport::new(plugin, engine.clone(), string_config_map(settings));
            transports.push((name, Arc::new(transport) as Arc<dyn Transport>));
        }
    }
    transports
}
