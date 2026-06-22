//! Logging setup for OxidRelay.
//!
//! Initialises a `tracing` subscriber with console output and a level filter.
//! The level comes from the configuration; `RUST_LOG` overrides it when set.
//! Platform integrations (journald, Windows Event Log, file logging) will be
//! added on top of this base.

use std::error::Error;

use tracing_subscriber::EnvFilter;

/// Builds the level filter, preferring `RUST_LOG`, then the configured level,
/// then a hard `info` fallback.
fn build_filter(level: &str) -> EnvFilter {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Initialises the global tracing subscriber. Returns an error if a subscriber
/// was already installed.
pub fn init(level: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(build_filter(level))
        .try_init()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_filter_accepts_known_levels() {
        for level in ["trace", "debug", "info", "warn", "error"] {
            // Building must not panic and must yield a usable filter.
            let _ = build_filter(level);
        }
    }

    #[test]
    fn build_filter_falls_back_on_garbage() {
        // Invalid directive falls back instead of panicking.
        let _ = build_filter("this is not a valid filter !!!");
    }
}
