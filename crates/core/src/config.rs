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
