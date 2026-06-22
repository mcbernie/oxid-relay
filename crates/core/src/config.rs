//! Configuration model and loading.
//!
//! The whole configuration is read from a single TOML file into plain serde
//! structs. This module is intentionally pure data plus loading and
//! validation; the actual use of the values happens in the respective crates
//! (transport, queue, logging, ...).
//!
//! Secrets are never stored in plain text. A field such as `password_env`
//! only holds the name of an environment variable; the value is resolved on
//! demand via the corresponding `password()` helper.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::message::Address;

/// Errors that can occur while loading or validating configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The config file could not be read from disk.
    #[error("could not read config file {path}: {source}")]
    Read {
        /// Path that was attempted.
        path: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// The TOML content could not be parsed.
    #[error("could not parse config: {0}")]
    Parse(#[from] toml::de::Error),

    /// The config parsed but failed semantic validation.
    #[error("invalid config: {0}")]
    Invalid(String),

    /// A referenced environment variable is not set.
    #[error("environment variable {0} is not set")]
    MissingEnv(String),
}

/// Convenience result alias for configuration operations.
pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

/// Reads a secret from the environment variable with the given name.
fn resolve_env(var: &str) -> ConfigResult<String> {
    std::env::var(var).map_err(|_| ConfigError::MissingEnv(var.to_string()))
}

/// Root configuration as read from the TOML file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Mail transport settings.
    #[serde(default)]
    pub mail: MailConfig,
    /// Queue / persistence settings.
    #[serde(default)]
    pub queue: QueueConfig,
    /// Logging settings.
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Subject prefix settings (sender labelling).
    #[serde(default)]
    pub subject: SubjectConfig,
    /// Network security settings (IP whitelist).
    #[serde(default)]
    pub security: SecurityConfig,
    /// Authentication / sender identification settings.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Per-plugin settings, keyed by plugin name. Each value is a flat table of
    /// string key/value pairs passed to the plugin script as its `config` map.
    #[serde(default)]
    pub plugins: BTreeMap<String, BTreeMap<String, String>>,
    /// Ingress (incoming mail) settings.
    #[serde(default)]
    pub ingress: IngressConfig,
    /// Routing rules: channel selection and recipient override by sender.
    #[serde(default)]
    pub routing: RoutingConfig,
    /// Dispatcher tuning (concurrency, polling, retry).
    #[serde(default)]
    pub dispatcher: DispatcherSettings,
}

impl Config {
    /// Loads and validates the configuration from a TOML file on disk.
    pub fn load(path: impl AsRef<Path>) -> ConfigResult<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_toml_str(&raw)
    }

    /// Parses and validates the configuration from a TOML string.
    pub fn from_toml_str(raw: &str) -> ConfigResult<Self> {
        let config: Config = toml::from_str(raw)?;
        config.validate()?;
        Ok(config)
    }

    /// Performs semantic validation beyond what serde guarantees.
    pub fn validate(&self) -> ConfigResult<()> {
        if self.queue.database.trim().is_empty() {
            return Err(ConfigError::Invalid("queue.database is empty".into()));
        }

        const LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];
        if !LEVELS.contains(&self.logging.level.as_str()) {
            return Err(ConfigError::Invalid(format!(
                "unknown logging.level '{}', expected one of {LEVELS:?}",
                self.logging.level
            )));
        }

        if let Some(smtp) = &self.mail.smtp {
            if smtp.host.trim().is_empty() {
                return Err(ConfigError::Invalid("mail.smtp.host is empty".into()));
            }
            if smtp.port == 0 {
                return Err(ConfigError::Invalid("mail.smtp.port must be non-zero".into()));
            }
        }

        // Dispatcher knobs must be positive; zero values would stall delivery.
        if self.dispatcher.batch_size == 0 {
            return Err(ConfigError::Invalid("dispatcher.batch_size must be >= 1".into()));
        }
        if self.dispatcher.concurrency == 0 {
            return Err(ConfigError::Invalid("dispatcher.concurrency must be >= 1".into()));
        }
        if self.dispatcher.poll_interval_secs == 0 {
            return Err(ConfigError::Invalid(
                "dispatcher.poll_interval_secs must be >= 1".into(),
            ));
        }
        if self.dispatcher.max_attempts == 0 {
            return Err(ConfigError::Invalid("dispatcher.max_attempts must be >= 1".into()));
        }

        // Every subject format must keep the original subject somewhere.
        if !self.subject.format.contains("%original%") {
            return Err(ConfigError::Invalid(
                "subject.format must contain %original%".into(),
            ));
        }
        for (name, sender) in &self.subject.senders {
            if !sender.format.contains("%original%") {
                return Err(ConfigError::Invalid(format!(
                    "subject.senders.\"{name}\".format must contain %original%"
                )));
            }
        }

        Ok(())
    }

    /// Returns the resolved settings for a plugin.
    ///
    /// Keys ending in `_env` are treated as secrets: their value names an
    /// environment variable, and the resolved value is exposed under the key
    /// without the `_env` suffix. All other keys pass through unchanged.
    pub fn plugin_settings(&self, name: &str) -> ConfigResult<BTreeMap<String, String>> {
        let mut resolved = BTreeMap::new();
        let Some(raw) = self.plugins.get(name) else {
            return Ok(resolved);
        };
        for (key, value) in raw {
            match key.strip_suffix("_env") {
                Some(stripped) => {
                    resolved.insert(stripped.to_string(), resolve_env(value)?);
                }
                None => {
                    resolved.insert(key.clone(), value.clone());
                }
            }
        }
        Ok(resolved)
    }
}

/// Dispatcher tuning. Times are in seconds. Mirrors the dispatcher's runtime
/// knobs; the binary maps this into the dispatcher's own config type.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatcherSettings {
    /// Maximum mails fetched per polling tick.
    #[serde(default = "default_batch_size")]
    pub batch_size: u32,
    /// Maximum mails delivered concurrently.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Delay between polling ticks, in seconds.
    #[serde(default = "default_poll_secs")]
    pub poll_interval_secs: u64,
    /// Time after which an in-flight mail is treated as orphaned, in seconds.
    #[serde(default = "default_lease_secs")]
    pub sending_lease_secs: u64,
    /// Maximum delivery attempts before a mail is buried as dead.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Base retry delay, in seconds.
    #[serde(default = "default_retry_base_secs")]
    pub retry_base_secs: u64,
    /// Maximum retry delay (backoff cap), in seconds.
    #[serde(default = "default_retry_max_secs")]
    pub retry_max_secs: u64,
}

impl Default for DispatcherSettings {
    fn default() -> Self {
        Self {
            batch_size: default_batch_size(),
            concurrency: default_concurrency(),
            poll_interval_secs: default_poll_secs(),
            sending_lease_secs: default_lease_secs(),
            max_attempts: default_max_attempts(),
            retry_base_secs: default_retry_base_secs(),
            retry_max_secs: default_retry_max_secs(),
        }
    }
}

fn default_batch_size() -> u32 {
    64
}
fn default_concurrency() -> usize {
    8
}
fn default_poll_secs() -> u64 {
    5
}
fn default_lease_secs() -> u64 {
    120
}
fn default_max_attempts() -> u32 {
    5
}
fn default_retry_base_secs() -> u64 {
    30
}
fn default_retry_max_secs() -> u64 {
    3600
}

/// Routing configuration: which channel (transport) handles a mail and an
/// optional recipient override, decided by the sender address.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    /// Action for senders without a specific rule. `None` means reject.
    #[serde(default)]
    pub default: Option<RouteRule>,
    /// Per-sender rules keyed by the envelope sender address.
    #[serde(default)]
    pub senders: BTreeMap<String, RouteRule>,
}

/// A single routing rule. Either a single `transport` (with optional
/// `recipients`) or a list of `targets` for fan-out to several channels.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRule {
    /// Single transport/channel name, or `"reject"` to refuse the mail.
    /// Ignored when `targets` is set.
    #[serde(default)]
    pub transport: Option<String>,
    /// Optional recipient override for the single-transport form. When
    /// non-empty, replaces the original recipients.
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Fan-out targets. When non-empty, the mail is delivered to every target.
    #[serde(default)]
    pub targets: Vec<RouteTargetConfig>,
}

/// One fan-out target inside a [`RouteRule`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteTargetConfig {
    /// Transport/channel name. A `"reject"` entry is skipped.
    pub transport: String,
    /// Optional recipient override for this target.
    #[serde(default)]
    pub recipients: Vec<String>,
}

/// A resolved delivery target: one transport and optional recipient override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteTarget {
    /// Transport/channel name.
    pub transport: String,
    /// Replacement recipients, or `None` to keep the original ones.
    pub recipients: Option<Vec<Address>>,
}

/// The resolved routing decision for a mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Refuse the mail.
    Reject,
    /// Deliver to one or more targets.
    Deliver(Vec<RouteTarget>),
}

/// Turns recipient strings into an optional address list (`None` if empty).
fn recipient_override(recipients: &[String]) -> Option<Vec<Address>> {
    if recipients.is_empty() {
        None
    } else {
        Some(recipients.iter().map(|r| Address::new(r.clone())).collect())
    }
}

impl RouteRule {
    /// Expands the rule into concrete delivery targets, dropping `"reject"`
    /// entries. An empty result means the rule rejects the mail.
    fn expand(&self) -> Vec<RouteTarget> {
        if !self.targets.is_empty() {
            return self
                .targets
                .iter()
                .filter(|target| !target.transport.eq_ignore_ascii_case("reject"))
                .map(|target| RouteTarget {
                    transport: target.transport.clone(),
                    recipients: recipient_override(&target.recipients),
                })
                .collect();
        }
        match &self.transport {
            Some(transport) if !transport.eq_ignore_ascii_case("reject") => vec![RouteTarget {
                transport: transport.clone(),
                recipients: recipient_override(&self.recipients),
            }],
            _ => Vec::new(),
        }
    }
}

impl RoutingConfig {
    /// Whether any routing rule is configured. When inactive, callers keep
    /// their previous behaviour instead of rejecting everything.
    pub fn is_active(&self) -> bool {
        self.default.is_some() || !self.senders.is_empty()
    }

    /// Resolves the route for a sender address. A per-sender rule wins over the
    /// default; a missing rule or a rule with no deliverable target rejects.
    pub fn resolve(&self, sender: &str) -> Route {
        let rule = self.senders.get(sender).or(self.default.as_ref());
        match rule {
            Some(rule) => {
                let targets = rule.expand();
                if targets.is_empty() {
                    Route::Reject
                } else {
                    Route::Deliver(targets)
                }
            }
            None => Route::Reject,
        }
    }
}

/// Ingress (incoming mail) settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressConfig {
    /// SMTP listener, if enabled. Presence of this section enables it.
    pub smtp: Option<SmtpIngressConfig>,
}

/// SMTP listener settings. The relay acts as an SMTP server on the LAN.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpIngressConfig {
    /// Socket address to listen on, e.g. `127.0.0.1:2525`.
    #[serde(default = "default_ingress_bind")]
    pub bind: String,
    /// Hostname announced in the SMTP greeting banner.
    #[serde(default = "default_ingress_hostname")]
    pub hostname: String,
}

fn default_ingress_bind() -> String {
    "127.0.0.1:2525".to_string()
}

fn default_ingress_hostname() -> String {
    "oxid-relay".to_string()
}

/// Mail transport settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailConfig {
    /// SMTP transport, if configured.
    pub smtp: Option<SmtpConfig>,
    /// Name of the transport used for mails that do not name one. Falls back to
    /// `smtp` when an SMTP transport is configured and this is unset.
    #[serde(default)]
    pub default_transport: Option<String>,
}

/// SMTP connection settings (e.g. Microsoft 365).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    /// SMTP host, e.g. `smtp.office365.com`.
    pub host: String,
    /// SMTP port, e.g. `587` for STARTTLS.
    pub port: u16,
    /// Login user name.
    pub username: String,
    /// Name of the environment variable holding the password.
    pub password_env: String,
}

impl SmtpConfig {
    /// Resolves the SMTP password from the configured environment variable.
    pub fn password(&self) -> ConfigResult<String> {
        resolve_env(&self.password_env)
    }
}

/// Queue / persistence settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueConfig {
    /// Path or URL of the SQLite database.
    #[serde(default = "default_database")]
    pub database: String,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            database: default_database(),
        }
    }
}

fn default_database() -> String {
    "queue.db".to_string()
}

/// Logging settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// Log level: trace, debug, info, warn or error.
    #[serde(default = "default_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_level(),
        }
    }
}

fn default_level() -> String {
    "info".to_string()
}

/// Subject prefix settings.
///
/// The relay prepends a sender label to the subject. The format supports the
/// placeholders `%name%` (sender identity) and `%original%` (incoming subject).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectConfig {
    /// Global default format applied to every sender.
    #[serde(default = "default_subject_format")]
    pub format: String,
    /// Per-sender overrides keyed by sender name.
    #[serde(default)]
    pub senders: BTreeMap<String, SenderSubject>,
}

impl Default for SubjectConfig {
    fn default() -> Self {
        Self {
            format: default_subject_format(),
            senders: BTreeMap::new(),
        }
    }
}

fn default_subject_format() -> String {
    "[Abs: %name%] %original%".to_string()
}

impl SubjectConfig {
    /// Returns the effective subject format for a sender, falling back to the
    /// global format if no per-sender override exists.
    pub fn format_for(&self, sender: &str) -> &str {
        self.senders
            .get(sender)
            .map(|s| s.format.as_str())
            .unwrap_or(&self.format)
    }

    /// Renders the prefixed subject for a sender by substituting the
    /// placeholders `%name%` and `%original%` in the effective format.
    pub fn render(&self, sender: &str, original: &str) -> String {
        self.format_for(sender)
            .replace("%name%", sender)
            .replace("%original%", original)
    }
}

/// Per-sender subject format override.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SenderSubject {
    /// Format string for this specific sender.
    pub format: String,
}

/// Network security settings. The relay is LAN-only; the whitelist is the
/// primary access control.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// Allowed source addresses / CIDR ranges. Empty means nothing is allowed.
    #[serde(default)]
    pub ip_whitelist: Vec<String>,
}

/// Authentication and sender identification settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// Anonymous mode: identify sender from the subject (mode A).
    #[serde(default)]
    pub anonymous: AnonymousAuth,
    /// Fixed credential blocks per service (mode B1).
    #[serde(default)]
    pub services: BTreeMap<String, ServiceAuth>,
    /// Self-registration from supplied credentials (mode B2).
    #[serde(default)]
    pub self_register: SelfRegisterAuth,
}

impl AuthConfig {
    /// Authenticates an incoming sender by username and password.
    ///
    /// Returns the resolved sender name on success, `None` on rejection:
    /// - Mode B1: a configured service whose username and password match
    ///   yields its key as the sender name.
    /// - Mode B2: if self-registration is enabled and no service matched, any
    ///   credentials are accepted and the username becomes the sender name.
    pub fn authenticate(&self, username: &str, password: &str) -> ConfigResult<Option<String>> {
        for (name, service) in &self.services {
            if service.username == username {
                // Username matched: password must match, otherwise reject.
                return if service.password()? == password {
                    Ok(Some(name.clone()))
                } else {
                    Ok(None)
                };
            }
        }
        if self.self_register.enabled {
            return Ok(Some(username.to_string()));
        }
        Ok(None)
    }
}

/// Anonymous mode settings (mode A).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnonymousAuth {
    /// Whether anonymous, whitelist-only delivery is allowed.
    #[serde(default)]
    pub enabled: bool,
    /// Pattern used to extract the sender name from the subject.
    #[serde(default)]
    pub subject_match: Option<String>,
}

/// Fixed credentials for a known service (mode B1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceAuth {
    /// Login user name expected from this service.
    pub username: String,
    /// Name of the environment variable holding the password.
    pub password_env: String,
}

impl ServiceAuth {
    /// Resolves the service password from the configured environment variable.
    pub fn password(&self) -> ConfigResult<String> {
        resolve_env(&self.password_env)
    }
}

/// Self-registration settings (mode B2).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelfRegisterAuth {
    /// Whether unknown credentials may register a new sender on first contact.
    #[serde(default)]
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_defaults() {
        let config = Config::from_toml_str("").expect("empty config is valid");
        assert_eq!(config.queue.database, "queue.db");
        assert_eq!(config.logging.level, "info");
        assert_eq!(config.subject.format, "[Abs: %name%] %original%");
        assert!(config.mail.smtp.is_none());
        assert!(config.security.ip_whitelist.is_empty());
    }

    #[test]
    fn parses_full_config() {
        let raw = r#"
            [mail.smtp]
            host = "smtp.office365.com"
            port = 587
            username = "relay@example.com"
            password_env = "MAIL_PASSWORD"

            [queue]
            database = "queue.db"

            [logging]
            level = "debug"

            [subject]
            format = "[Abs: %name%] %original%"

            [subject.senders."Server01.company.local"]
            format = "[%name%] %original%"

            [security]
            ip_whitelist = ["10.0.0.0/8", "192.168.0.0/16"]

            [auth.anonymous]
            enabled = true
            subject_match = "^(?P<name>Server ?\\d+):"

            [auth.services."backup-host"]
            username = "backup"
            password_env = "BACKUP_PASSWORD"

            [auth.self_register]
            enabled = false
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");

        let smtp = config.mail.smtp.expect("smtp present");
        assert_eq!(smtp.host, "smtp.office365.com");
        assert_eq!(smtp.port, 587);
        assert_eq!(config.logging.level, "debug");
        assert_eq!(config.security.ip_whitelist.len(), 2);
        assert!(config.auth.anonymous.enabled);
        assert!(config.auth.services.contains_key("backup-host"));
    }

    #[test]
    fn format_for_uses_override_then_falls_back() {
        let raw = r#"
            [subject]
            format = "[Abs: %name%] %original%"
            [subject.senders."special"]
            format = "<%name%> %original%"
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        assert_eq!(config.subject.format_for("special"), "<%name%> %original%");
        assert_eq!(
            config.subject.format_for("unknown"),
            "[Abs: %name%] %original%"
        );
    }

    #[test]
    fn rejects_unknown_log_level() {
        let raw = r#"
            [logging]
            level = "verbose"
        "#;
        assert!(matches!(
            Config::from_toml_str(raw),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_subject_format_without_original() {
        let raw = r#"
            [subject]
            format = "[Abs: %name%]"
        "#;
        assert!(matches!(
            Config::from_toml_str(raw),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_zero_smtp_port() {
        let raw = r#"
            [mail.smtp]
            host = "smtp.example.com"
            port = 0
            username = "u"
            password_env = "P"
        "#;
        assert!(matches!(
            Config::from_toml_str(raw),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_unknown_field() {
        let raw = r#"
            [queue]
            databse = "typo.db"
        "#;
        assert!(matches!(
            Config::from_toml_str(raw),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn plugin_settings_passthrough_and_env_resolution() {
        let raw = r#"
            [plugins.graph]
            tenant_id = "tenant-123"
            client_secret_env = "OXID_TEST_GRAPH_SECRET"
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");

        // SAFETY: test-only, single-threaded access to a uniquely named var.
        unsafe {
            std::env::set_var("OXID_TEST_GRAPH_SECRET", "s3cr3t");
        }
        let settings = config.plugin_settings("graph").expect("settings");
        assert_eq!(settings.get("tenant_id").map(String::as_str), Some("tenant-123"));
        assert_eq!(settings.get("client_secret").map(String::as_str), Some("s3cr3t"));
    }

    #[test]
    fn subject_render_substitutes_placeholders() {
        let config = Config::from_toml_str("").expect("valid config");
        assert_eq!(
            config.subject.render("Server01", "Status Okay"),
            "[Abs: Server01] Status Okay"
        );
    }

    #[test]
    fn authenticate_self_register_accepts_any() {
        let raw = r#"
            [auth.self_register]
            enabled = true
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        assert_eq!(
            config.auth.authenticate("monitoring", "whatever").expect("auth"),
            Some("monitoring".to_string())
        );
    }

    #[test]
    fn authenticate_service_matches_name_and_password() {
        let raw = r#"
            [auth.services."backup-host"]
            username = "backup"
            password_env = "OXID_TEST_BACKUP_PW"
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        // SAFETY: test-only, uniquely named variable.
        unsafe {
            std::env::set_var("OXID_TEST_BACKUP_PW", "geheim");
        }
        assert_eq!(
            config.auth.authenticate("backup", "geheim").expect("auth"),
            Some("backup-host".to_string())
        );
        // Wrong password is rejected.
        assert_eq!(config.auth.authenticate("backup", "falsch").expect("auth"), None);
        // Unknown user with self-registration disabled is rejected.
        assert_eq!(config.auth.authenticate("fremd", "x").expect("auth"), None);
    }

    #[test]
    fn routing_inactive_when_empty() {
        let config = Config::from_toml_str("").expect("valid config");
        assert!(!config.routing.is_active());
    }

    #[test]
    fn routing_sender_rule_overrides_recipients_and_transport() {
        let raw = r#"
            [routing.senders."bla@teams"]
            transport = "teams"
            recipients = ["ops-channel@teams", "oncall@teams"]
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        assert!(config.routing.is_active());
        match config.routing.resolve("bla@teams") {
            Route::Deliver(targets) => {
                assert_eq!(targets.len(), 1);
                assert_eq!(targets[0].transport, "teams");
                let recipients = targets[0].recipients.as_ref().expect("override");
                assert_eq!(recipients.len(), 2);
                assert_eq!(recipients[0].email, "ops-channel@teams");
            }
            Route::Reject => panic!("should deliver"),
        }
    }

    #[test]
    fn routing_fans_out_to_multiple_targets() {
        let raw = r#"
            [routing.senders."alarm@x"]
            targets = [
                { transport = "teams", recipients = ["ops@teams.local"] },
                { transport = "ntfy" },
                { transport = "reject" },
            ]
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        match config.routing.resolve("alarm@x") {
            Route::Deliver(targets) => {
                // The "reject" entry is dropped.
                assert_eq!(targets.len(), 2);
                assert_eq!(targets[0].transport, "teams");
                assert_eq!(targets[0].recipients.as_ref().unwrap()[0].email, "ops@teams.local");
                assert_eq!(targets[1].transport, "ntfy");
                assert!(targets[1].recipients.is_none());
            }
            Route::Reject => panic!("should deliver"),
        }
    }

    #[test]
    fn routing_default_applies_and_reject_works() {
        let raw = r#"
            [routing]
            [routing.default]
            transport = "graph"
            [routing.senders."blocked@x"]
            transport = "reject"
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        // Unknown sender falls back to default transport.
        assert_eq!(
            config.routing.resolve("anyone@x"),
            Route::Deliver(vec![RouteTarget {
                transport: "graph".to_string(),
                recipients: None,
            }])
        );
        // Explicit reject rule.
        assert_eq!(config.routing.resolve("blocked@x"), Route::Reject);
    }

    #[test]
    fn routing_rejects_unknown_sender_without_default() {
        let raw = r#"
            [routing.senders."known@x"]
            transport = "graph"
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        assert_eq!(config.routing.resolve("unknown@x"), Route::Reject);
    }

    #[test]
    fn dispatcher_defaults_and_overrides() {
        let config = Config::from_toml_str("").expect("valid config");
        assert_eq!(config.dispatcher.concurrency, 8);
        assert_eq!(config.dispatcher.max_attempts, 5);

        let raw = r#"
            [dispatcher]
            concurrency = 16
            poll_interval_secs = 2
            max_attempts = 3
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        assert_eq!(config.dispatcher.concurrency, 16);
        assert_eq!(config.dispatcher.poll_interval_secs, 2);
        assert_eq!(config.dispatcher.max_attempts, 3);
        // Untouched fields keep their defaults.
        assert_eq!(config.dispatcher.batch_size, 64);
    }

    #[test]
    fn dispatcher_rejects_zero_concurrency() {
        let raw = r#"
            [dispatcher]
            concurrency = 0
        "#;
        assert!(matches!(
            Config::from_toml_str(raw),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn mail_default_transport_parses() {
        let raw = r#"
            [mail]
            default_transport = "graph"
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        assert_eq!(config.mail.default_transport.as_deref(), Some("graph"));
    }

    #[test]
    fn ingress_smtp_defaults_when_section_present() {
        let raw = r#"
            [ingress.smtp]
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        let smtp = config.ingress.smtp.expect("smtp ingress");
        assert_eq!(smtp.bind, "127.0.0.1:2525");
        assert_eq!(smtp.hostname, "oxid-relay");
    }

    #[test]
    fn plugin_settings_unknown_plugin_is_empty() {
        let config = Config::from_toml_str("").expect("valid config");
        assert!(config.plugin_settings("nope").expect("settings").is_empty());
    }

    #[test]
    fn plugin_settings_missing_env_reports_error() {
        let raw = r#"
            [plugins.graph]
            client_secret_env = "OXID_TEST_DEFINITELY_UNSET"
        "#;
        let config = Config::from_toml_str(raw).expect("valid config");
        assert!(matches!(
            config.plugin_settings("graph"),
            Err(ConfigError::MissingEnv(_))
        ));
    }

    #[test]
    fn password_missing_env_reports_error() {
        let smtp = SmtpConfig {
            host: "h".into(),
            port: 587,
            username: "u".into(),
            password_env: "OXID_RELAY_DEFINITELY_UNSET_VAR".into(),
        };
        assert!(matches!(smtp.password(), Err(ConfigError::MissingEnv(_))));
    }
}
