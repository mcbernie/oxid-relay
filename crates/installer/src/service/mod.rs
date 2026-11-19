//! Platform service managers behind one trait.

use crate::layout::InstallLayout;

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
}

/// The service manager for the platform this binary runs on.
pub fn current() -> anyhow::Result<Box<dyn ServiceManager>> {
    todo!("installer agent: systemd on Linux, SCM on Windows, error on macOS")
}
