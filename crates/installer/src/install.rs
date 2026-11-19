//! `oxid-relay install` / `oxid-relay uninstall`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::SERVICE_NAME;
use crate::bundle::{Bundle, CopyOptions};
use crate::layout::InstallLayout;
use crate::service;

/// Options for [`install`].
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Register the service but do not start it.
    pub no_start: bool,
}

/// Installs the bundle the running binary came from as a system service.
///
/// Safe to re-run: a running service is stopped first, the binary and
/// bundled plugins are replaced, an existing `config.toml` is kept and the
/// service registration is updated.
pub fn install(options: &InstallOptions) -> anyhow::Result<()> {
    service::ensure_admin()?;
    let layout = InstallLayout::current()?;
    let bundle = Bundle::from_current_exe()?;
    let manager = service::current()?;

    if manager.is_running()? {
        println!("Stopping the running {SERVICE_NAME} service...");
        manager.stop()?;
    }

    let had_config = layout.config_file.exists();
    println!("Copying files from {}...", bundle.root.display());
    bundle.copy_into(
        &layout,
        CopyOptions {
            binary: true,
            seed_config: true,
        },
    )?;

    println!("Registering the {SERVICE_NAME} service...");
    manager.register(&layout)?;

    if options.no_start {
        println!("Service registered but not started (--no-start).");
    } else {
        println!("Starting the service...");
        manager.start()?;
    }

    print_next_steps(&layout, had_config, options.no_start);
    Ok(())
}

/// Removes the service and binary; with `purge` also config, secrets and data.
pub fn uninstall(purge: bool) -> anyhow::Result<()> {
    service::ensure_admin()?;
    let layout = InstallLayout::current()?;
    let manager = service::current()?;

    println!("Stopping and removing the {SERVICE_NAME} service...");
    manager.stop()?;
    manager.unregister()?;

    remove_binary(&layout)?;

    if purge {
        for dir in purge_dirs(&layout) {
            if dir.exists() {
                std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("removing {}", dir.display()))?;
                println!("Removed {}", dir.display());
            }
        }
        if let Err(err) = manager.purge() {
            println!("Warning: {err:#}");
        }
        println!("OxidRelay has been removed completely.");
    } else {
        println!("OxidRelay has been uninstalled. Kept (remove with `uninstall --purge`):");
        println!("  configuration: {}", layout.config_dir.display());
        println!("  plugins:       {}", layout.plugin_dir.display());
        println!("  data (queue):  {}", layout.data_dir.display());
    }
    Ok(())
}

/// Directories `--purge` removes, without duplicates or nested entries
/// (on Windows the plugin and data directories live in the config directory).
fn purge_dirs(layout: &InstallLayout) -> Vec<PathBuf> {
    let candidates = [&layout.config_dir, &layout.plugin_dir, &layout.data_dir];
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in candidates {
        if !candidates
            .iter()
            .any(|other| *other != dir && dir.starts_with(other))
            && !dirs.contains(dir)
        {
            dirs.push(dir.clone());
        }
    }
    dirs
}

/// Deletes the installed binary. On Windows the running executable cannot be
/// deleted, so it is renamed to `*.old` and the user is told to remove it.
fn remove_binary(layout: &InstallLayout) -> anyhow::Result<()> {
    let binary = &layout.binary;
    if !binary.exists() {
        return Ok(());
    }
    if cfg!(windows) && is_current_exe(binary) {
        let old = binary.with_extension("exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(binary, &old)
            .with_context(|| format!("renaming the running binary {}", binary.display()))?;
        println!(
            "The running binary was moved to {}; delete it once this command has exited.",
            old.display()
        );
        return Ok(());
    }
    std::fs::remove_file(binary).with_context(|| format!("removing {}", binary.display()))?;
    println!("Removed {}", binary.display());
    if cfg!(windows) {
        // Program Files\OxidRelay is ours; remove it if nothing else is left.
        let _ = std::fs::remove_dir(&layout.bin_dir);
    }
    Ok(())
}

/// Whether `path` is the executable of this process.
fn is_current_exe(path: &Path) -> bool {
    let canonical = |p: &Path| p.canonicalize().ok();
    match std::env::current_exe() {
        Ok(exe) => canonical(&exe).is_some() && canonical(&exe) == canonical(path),
        Err(_) => false,
    }
}

/// Tells the user where things live and how to proceed.
fn print_next_steps(layout: &InstallLayout, had_config: bool, no_start: bool) {
    println!();
    println!("OxidRelay {} is installed.", crate::VERSION);
    println!("  binary:        {}", layout.binary.display());
    if had_config {
        println!("  configuration: {} (kept)", layout.config_file.display());
    } else {
        println!(
            "  configuration: {} (created from config.example.toml, edit it)",
            layout.config_file.display()
        );
    }
    println!("  plugins:       {}", layout.plugin_dir.display());
    println!("  data (queue):  {}", layout.data_dir.display());
    println!();

    if cfg!(windows) {
        println!("Secrets (CLIENT_SECRET_AZURE, TEAMS_WEBHOOK_URL, ...) are read from machine");
        println!("environment variables, e.g. in an elevated PowerShell:");
        println!(
            "  [Environment]::SetEnvironmentVariable('CLIENT_SECRET_AZURE', '<secret>', 'Machine')"
        );
        println!();
        println!("After changing the configuration or secrets:  Restart-Service {SERVICE_NAME}");
        println!("Service status:                               Get-Service {SERVICE_NAME}");
        println!("Logs: Event Viewer > Windows Logs > Application");
    } else {
        if let Some(env_file) = &layout.env_file {
            println!(
                "Secrets (CLIENT_SECRET_AZURE, TEAMS_WEBHOOK_URL, ...) go into {} (mode 0600).",
                env_file.display()
            );
        }
        println!(
            "After changing the configuration or secrets:  sudo systemctl restart {SERVICE_NAME}"
        );
        println!("Service status:                               systemctl status {SERVICE_NAME}");
        println!("Logs:                                         journalctl -u {SERVICE_NAME} -f");
    }
    if no_start {
        println!();
        println!("Start the service once configured.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_dirs_skip_nested_and_duplicates() {
        let windows = InstallLayout::windows(Path::new("PF"), Path::new("PD"));
        assert_eq!(purge_dirs(&windows), vec![windows.config_dir.clone()]);

        let linux = InstallLayout::linux();
        assert_eq!(
            purge_dirs(&linux),
            vec![linux.config_dir.clone(), linux.data_dir.clone()]
        );
    }
}
