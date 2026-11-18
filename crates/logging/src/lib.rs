//! Logging setup for OxidRelay.
//!
//! Initialises a `tracing` subscriber with console output and a level filter.
//! The level comes from the configuration; `RUST_LOG` overrides it when set.
//! Platform integrations (journald, Windows Event Log, file logging) will be
//! added on top of this base.

use std::error::Error;
use std::io::IsTerminal;

use oxid_relay_core::config::ColorMode;
use tracing_subscriber::EnvFilter;

/// Pure colour decision.
///
/// `no_color` is true when `NO_COLOR` is set to a non-empty value, `force` when
/// `FORCE_COLOR` / `CLICOLOR_FORCE` request colour, and `vt_enabled` whether
/// the terminal can process ANSI escapes (always true off Windows). An explicit
/// `always`/`never` wins; in `auto`, `NO_COLOR` beats `force`, and without
/// either colour needs an interactive, ANSI-capable terminal.
fn decide_ansi(
    mode: ColorMode,
    is_terminal: bool,
    no_color: bool,
    force: bool,
    vt_enabled: bool,
) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto if no_color => false,
        ColorMode::Auto if force => vt_enabled,
        ColorMode::Auto => is_terminal && vt_enabled,
    }
}

/// True when the environment variable is set to a non-empty value.
fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// True when `FORCE_COLOR` / `CLICOLOR_FORCE` is set to something other than `0`.
fn env_force() -> bool {
    ["FORCE_COLOR", "CLICOLOR_FORCE"]
        .iter()
        .any(|n| std::env::var(n).is_ok_and(|v| !v.is_empty() && v != "0"))
}

/// Tries to enable ANSI processing on the Windows console; true on success.
#[cfg(windows)]
fn enable_vt() -> bool {
    enable_ansi_support::enable_ansi_support().is_ok()
}

/// Other platforms process ANSI escapes natively.
#[cfg(not(windows))]
fn enable_vt() -> bool {
    true
}

/// Whether to emit ANSI colour codes, based on config and environment.
fn use_ansi(mode: ColorMode) -> bool {
    // Skip the console probe when the mode already decides the outcome.
    let vt = mode != ColorMode::Never;
    let vt_enabled = vt && enable_vt();
    decide_ansi(
        mode,
        std::io::stdout().is_terminal(),
        env_set("NO_COLOR"),
        env_force(),
        vt_enabled,
    )
}

/// Builds the level filter, preferring `RUST_LOG`, then the configured level,
/// then a hard `info` fallback.
fn build_filter(level: &str) -> EnvFilter {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Initialises the global tracing subscriber. Returns an error if a subscriber
/// was already installed.
pub fn init(level: &str, color: ColorMode) -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_ansi(use_ansi(color))
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

    #[test]
    fn explicit_modes_ignore_environment() {
        for (tty, no_color, vt) in [(false, true, false), (true, false, true)] {
            assert!(decide_ansi(ColorMode::Always, tty, no_color, false, vt));
            assert!(!decide_ansi(ColorMode::Never, tty, no_color, true, vt));
        }
    }

    #[test]
    fn auto_requires_terminal_and_vt() {
        assert!(decide_ansi(ColorMode::Auto, true, false, false, true));
        assert!(!decide_ansi(ColorMode::Auto, false, false, false, true));
        // Windows console where VT could not be enabled.
        assert!(!decide_ansi(ColorMode::Auto, true, false, false, false));
    }

    #[test]
    fn auto_honours_no_color_over_force() {
        assert!(!decide_ansi(ColorMode::Auto, true, true, false, true));
        assert!(!decide_ansi(ColorMode::Auto, true, true, true, true));
    }

    #[test]
    fn auto_force_colours_pipes_but_needs_vt() {
        assert!(decide_ansi(ColorMode::Auto, false, false, true, true));
        assert!(!decide_ansi(ColorMode::Auto, false, false, true, false));
    }
}
