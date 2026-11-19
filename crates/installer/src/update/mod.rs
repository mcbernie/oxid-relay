//! `oxid-relay update`: fetch, verify and apply the latest GitHub release.

/// Options for [`update`].
#[derive(Debug, Clone, Default)]
pub struct UpdateOptions {
    /// Only report installed and available versions.
    pub check: bool,
    /// Reinstall even if the installed version is current or newer.
    pub force: bool,
    /// Install this release tag (e.g. `v0.2.0`) instead of the latest.
    pub version: Option<String>,
}

/// Runs the update according to `options`.
pub fn update(options: &UpdateOptions) -> anyhow::Result<()> {
    todo!("updater agent: {options:?}")
}
