//! OxidRelay binary entry point.
//!
//! Wires together CLI, configuration, logging, the queue, the transports
//! (SMTP plus Rhai plugins) and the background dispatcher.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use fs4::FileExt;
use oxid_relay_core::{Config, Queue, Transport};
use oxid_relay_dispatcher::{Dispatcher, DispatcherConfig, RetryPolicy};
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

    /// Run under the Windows service control manager. Set automatically by the
    /// installed service; not meant for interactive use.
    #[cfg(windows)]
    #[arg(long, hide = true)]
    service: bool,
}

fn main() -> anyhow::Result<()> {
    load_dotenv();
    let cli = Cli::parse();

    #[cfg(windows)]
    if cli.service {
        return windows_service_runner::run(cli.config);
    }

    run_foreground(cli.config)
}

/// Runs the relay in the foreground (console, systemd or launchd), stopping on
/// SIGINT or, on Unix, SIGTERM.
fn run_foreground(config_path: PathBuf) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building async runtime")?;
    runtime.block_on(run_relay(config_path, shutdown_signal()))
}

/// Loads the configuration and runs the full relay (queue, transports, ingress
/// and dispatcher) until the `shutdown` future resolves.
async fn run_relay(
    config_path: PathBuf,
    shutdown: impl std::future::Future<Output = ()>,
) -> anyhow::Result<()> {
    let config = Arc::new(
        Config::load(&config_path)
            .with_context(|| format!("loading config from {}", config_path.display()))?,
    );

    oxid_relay_logging::init(&config.logging.level)
        .map_err(|err| anyhow::anyhow!("initialising logging: {err}"))?;

    tracing::info!(
        config = %config_path.display(),
        queue = %config.queue.database,
        log_level = %config.logging.level,
        "OxidRelay started"
    );

    // Refuse to start a second instance against the same queue.
    let _instance_lock = acquire_instance_lock(&config.queue.database)?;

    // Durable queue.
    let db_url = sqlite_url(&config.queue.database);
    let queue: Arc<dyn Queue> = Arc::new(
        SqliteQueue::connect(&db_url)
            .await
            .with_context(|| format!("opening queue {db_url}"))?,
    );

    // Available transports keyed by name.
    let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
    // Default transport: explicit config wins, otherwise SMTP when present.
    let mut default_transport = config.mail.default_transport.clone();

    if let Some(smtp) = &config.mail.smtp {
        match SmtpTransport::new(smtp) {
            Ok(transport) => {
                transports.insert("smtp".to_string(), Arc::new(transport));
                if default_transport.is_none() {
                    default_transport = Some("smtp".to_string());
                }
                tracing::info!(host = %smtp.host, "smtp transport ready");
            }
            Err(err) => tracing::warn!(error = %err, "smtp transport unavailable"),
        }
    }

    for (name, transport) in build_plugin_transports(config.as_ref()) {
        tracing::info!(plugin = %name, "plugin transport ready");
        transports.insert(name, transport);
    }

    if transports.is_empty() {
        tracing::warn!("no transports configured; mails cannot be delivered");
    }
    // A default transport that was never built (e.g. a plugin skipped due to a
    // missing secret) would silently fail every untargeted mail. Surface it now.
    if let Some(name) = &default_transport {
        if !transports.contains_key(name) {
            tracing::error!(
                default = %name,
                "default transport not available; check the transport config and required secrets"
            );
        }
    }

    // SMTP ingress runs on its own thread (mailin uses blocking IO). It shares
    // the queue and enqueues incoming mail via the runtime handle.
    if config.ingress.smtp.is_some() {
        let ingress_config = config.clone();
        let ingress_queue = queue.clone();
        let handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            if let Err(err) = oxid_relay_ingress_smtp::serve(ingress_config, ingress_queue, handle)
            {
                tracing::error!(error = %err, "smtp ingress stopped");
            }
        });
    }

    // Background dispatcher; runs until shutdown.
    let dispatcher = Dispatcher::new(
        queue,
        transports,
        default_transport,
        dispatcher_config(&config),
    );
    tracing::info!("dispatcher running");
    dispatcher.run(shutdown).await;

    tracing::info!("OxidRelay stopped");
    Ok(())
}

/// Loads a local `.env` file in debug builds so secrets are available without
/// exporting them by hand. No-op in release builds and when no file exists.
/// Logging is not up yet here, so this reports via stderr.
fn load_dotenv() {
    #[cfg(debug_assertions)]
    if let Ok(path) = dotenvy::dotenv() {
        eprintln!("loaded environment from {}", path.display());
    }
}

/// Maps the configured dispatcher settings into the dispatcher's config type.
fn dispatcher_config(config: &Config) -> DispatcherConfig {
    let settings = &config.dispatcher;
    DispatcherConfig {
        batch_size: settings.batch_size,
        concurrency: settings.concurrency,
        poll_interval: Duration::from_secs(settings.poll_interval_secs),
        sending_lease: Duration::from_secs(settings.sending_lease_secs),
        retry: RetryPolicy {
            max_attempts: settings.max_attempts,
            base_delay: Duration::from_secs(settings.retry_base_secs),
            max_delay: Duration::from_secs(settings.retry_max_secs),
        },
    }
}

/// Resolves when the process should shut down: SIGINT (Ctrl-C) or, on Unix,
/// SIGTERM (sent by systemd and launchd on stop).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Acquires an exclusive lock tied to the queue database so only one instance
/// runs against it. The returned file handle must be kept alive for the whole
/// process; dropping it (on exit) releases the lock. In-memory databases need
/// no lock.
fn acquire_instance_lock(database: &str) -> anyhow::Result<Option<std::fs::File>> {
    if database.contains(":memory:") || database.trim().is_empty() {
        return Ok(None);
    }
    let path = format!("{database}.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening lock file {path}"))?;

    match FileExt::try_lock(&file) {
        Ok(()) => Ok(Some(file)),
        Err(fs4::TryLockError::WouldBlock) => anyhow::bail!(
            "another OxidRelay instance is already running on this queue (lock {path} is held)"
        ),
        Err(fs4::TryLockError::Error(err)) => {
            Err(anyhow::Error::from(err).context(format!("locking {path}")))
        }
    }
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

/// Windows service integration: runs the relay under the service control
/// manager, reporting status and stopping cleanly on a Stop/Shutdown control.
#[cfg(windows)]
mod windows_service_runner {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::sync::mpsc;
    use std::time::Duration;

    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    const SERVICE_NAME: &str = "oxid-relay";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    // The service entry point has a fixed signature, so the config path is
    // stashed here before the dispatcher takes over.
    static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

    define_windows_service!(ffi_service_main, service_main);

    /// Hands control to the service control manager. Returns once the service
    /// has stopped.
    pub fn run(config_path: PathBuf) -> anyhow::Result<()> {
        let _ = CONFIG_PATH.set(config_path);
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .map_err(|err| anyhow::anyhow!("starting service dispatcher: {err}"))?;
        Ok(())
    }

    fn service_main(_arguments: Vec<OsString>) {
        if let Err(err) = run_service() {
            // A subscriber may not be installed yet; fall back to stderr.
            eprintln!("oxid-relay service error: {err}");
        }
    }

    fn run_service() -> anyhow::Result<()> {
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        let event_handler = move |control| -> ServiceControlHandlerResult {
            match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = shutdown_tx.send(());
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };

        let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)
            .map_err(|err| anyhow::anyhow!("registering control handler: {err}"))?;

        status_handle
            .set_service_status(status(
                ServiceState::Running,
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            ))
            .map_err(|err| anyhow::anyhow!("setting running status: {err}"))?;

        let config_path = CONFIG_PATH
            .get()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("config.toml"));

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|err| anyhow::anyhow!("building async runtime: {err}"))?;

        // Bridge the synchronous control handler into an async shutdown future.
        let shutdown = async move {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = shutdown_rx.recv();
            })
            .await;
        };

        let result = runtime.block_on(super::run_relay(config_path, shutdown));

        let _ = status_handle
            .set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()));
        result
    }

    /// Builds a service status with no pending-state fields set.
    fn status(state: ServiceState, controls: ServiceControlAccept) -> ServiceStatus {
        ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: state,
            controls_accepted: controls,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        }
    }
}
