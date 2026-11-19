//! Install, uninstall and self-update support for OxidRelay.
//!
//! - [`install`] / [`uninstall`] set the relay up as a system service from an
//!   unpacked release bundle (systemd on Linux, the service control manager on
//!   Windows).
//! - [`update`] fetches the latest GitHub release, verifies it and swaps the
//!   installed binary and bundled plugins, rolling back if the service fails
//!   to start.
//!
//! Release bundle contract (produced by `.github/workflows/release.yml`):
//! - asset `oxid-relay-v{version}-{target}.tar.gz` (`.zip` on Windows),
//! - archive root holds `oxid-relay[.exe]`, `plugins/`, `config.example.toml`,
//!   `README.txt` (no top-level directory),
//! - a `SHA256SUMS` asset with lines `<hex sha256>  <asset name>`.

pub mod bundle;
mod install;
pub mod layout;
pub mod service;
pub mod update;

pub use install::{InstallOptions, install, uninstall};
pub use layout::InstallLayout;
pub use update::{UpdateOptions, update};

/// Service name used by systemd (`oxid-relay.service`) and the Windows SCM.
/// Must match the name the Windows service runner registers with.
pub const SERVICE_NAME: &str = "oxid-relay";

/// Human-readable service description.
pub const SERVICE_DESCRIPTION: &str = "OxidRelay mail relay and notification gateway";

/// GitHub repository (`owner/name`) releases are fetched from.
pub const GITHUB_REPO: &str = "mcbernie/oxid-relay";

/// Version of the running binary.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Target triple the running binary was built for.
pub const TARGET: &str = env!("OXID_RELAY_TARGET");
