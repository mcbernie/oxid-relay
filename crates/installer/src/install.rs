//! `oxid-relay install` / `oxid-relay uninstall`.

/// Options for [`install`].
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Register the service but do not start it.
    pub no_start: bool,
}

/// Installs the bundle the running binary came from as a system service.
pub fn install(options: &InstallOptions) -> anyhow::Result<()> {
    todo!("installer agent: {options:?}")
}

/// Removes the service and binary; with `purge` also config, secrets and data.
pub fn uninstall(purge: bool) -> anyhow::Result<()> {
    todo!("installer agent: purge={purge}")
}
