//! Platform service managers behind one trait.
//!
//! - [`systemd`] on Linux,
//! - [`windows`] (the service control manager) on Windows.
//!
//! [`command`] holds the external-command abstraction the systemd backend
//! uses, [`privilege`] the administrator check.

pub mod command;
pub mod privilege;
#[cfg(unix)]
pub mod systemd;
#[cfg(windows)]
pub mod windows;

use crate::layout::InstallLayout;

pub use privilege::ensure_admin;

/// Registers and controls the relay as a system service.
pub trait ServiceManager {
    /// Registers the service (autostart enabled) for the given layout.
    /// Idempotent: re-registering an existing service updates it.
    fn register(&self, layout: &InstallLayout) -> anyhow::Result<()>;
    /// Removes the service registration. Succeeds if it does not exist.
    fn unregister(&self) -> anyhow::Result<()>;
    /// Starts the service.
    fn start(&self) -> anyhow::Result<()>;
    /// Stops the service. Succeeds if it is not running.
    fn stop(&self) -> anyhow::Result<()>;
    /// Whether the service is currently running.
    fn is_running(&self) -> anyhow::Result<bool>;
    /// Whether the service is registered at all.
    fn is_registered(&self) -> anyhow::Result<bool>;
    /// Removes platform extras created by [`register`](Self::register) that
    /// outlive the registration, such as the Linux service account. Called by
    /// `uninstall --purge` after [`unregister`](Self::unregister).
    fn purge(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// The service manager for the platform this binary runs on.
pub fn current() -> anyhow::Result<Box<dyn ServiceManager>> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows::WindowsServices))
    }
    #[cfg(target_os = "macos")]
    {
        anyhow::bail!("installing as a service is not supported on macOS yet")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Ok(Box::new(systemd::Systemd::detect()?))
    }
}
