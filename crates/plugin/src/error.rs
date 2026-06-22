//! Error type for plugin loading and execution.

use thiserror::Error;

/// Errors that can occur while loading or running a plugin.
#[derive(Debug, Error)]
pub enum PluginError {
    /// A file could not be read.
    #[error("could not read {path}: {source}")]
    Io {
        /// Path that was attempted.
        path: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// The `plugin.toml` manifest could not be parsed.
    #[error("invalid manifest: {0}")]
    Manifest(#[from] toml::de::Error),

    /// The Rhai script failed to compile.
    #[error("script compile error: {0}")]
    Compile(String),

    /// The plugin failed semantic validation.
    #[error("invalid plugin: {0}")]
    Invalid(String),
}
