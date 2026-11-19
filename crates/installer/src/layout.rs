//! Standard installation paths per platform.
//!
//! These must stay in sync with the defaults the relay itself uses, notably
//! `oxid_relay_plugin::plugin_dirs()` for the plugin directory.

use std::path::{Path, PathBuf};

/// Where an installation lives on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallLayout {
    /// Directory holding the binary.
    pub bin_dir: PathBuf,
    /// Full path of the installed binary.
    pub binary: PathBuf,
    /// Directory holding `config.toml` (and on Linux the env file).
    pub config_dir: PathBuf,
    /// Full path of the active configuration file.
    pub config_file: PathBuf,
    /// Directory the relay loads plugins from.
    pub plugin_dir: PathBuf,
    /// Working directory of the service; the queue database lives here.
    pub data_dir: PathBuf,
    /// Secrets file read by systemd (`None` on Windows, which uses machine
    /// environment variables instead).
    pub env_file: Option<PathBuf>,
}

impl InstallLayout {
    /// Layout for Linux and other non-macOS Unix systems.
    pub fn linux() -> Self {
        let bin_dir = PathBuf::from("/usr/local/bin");
        let config_dir = PathBuf::from("/etc/oxid-relay");
        Self {
            binary: bin_dir.join("oxid-relay"),
            bin_dir,
            config_file: config_dir.join("config.toml"),
            plugin_dir: config_dir.join("plugins"),
            env_file: Some(config_dir.join("oxid-relay.env")),
            config_dir,
            data_dir: PathBuf::from("/var/lib/oxid-relay"),
        }
    }

    /// Layout for Windows, rooted at the given `Program Files` and
    /// `ProgramData` directories.
    pub fn windows(program_files: &Path, program_data: &Path) -> Self {
        let bin_dir = program_files.join("OxidRelay");
        let data = program_data.join("OxidRelay");
        Self {
            binary: bin_dir.join("oxid-relay.exe"),
            bin_dir,
            config_file: data.join("config.toml"),
            plugin_dir: data.join("plugins"),
            env_file: None,
            config_dir: data.clone(),
            data_dir: data,
        }
    }

    /// Layout for the platform this binary runs on, or an error where
    /// installation is not supported (macOS for now).
    pub fn current() -> anyhow::Result<Self> {
        if cfg!(windows) {
            let env_or = |name: &str, fallback: &str| {
                std::env::var_os(name)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(fallback))
            };
            Ok(Self::windows(
                &env_or("ProgramFiles", r"C:\Program Files"),
                &env_or("ProgramData", r"C:\ProgramData"),
            ))
        } else if cfg!(target_os = "macos") {
            anyhow::bail!("installing as a service is not supported on macOS yet")
        } else {
            Ok(Self::linux())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_layout_matches_plugin_default_and_unit() {
        let layout = InstallLayout::linux();
        assert_eq!(layout.binary, PathBuf::from("/usr/local/bin/oxid-relay"));
        assert_eq!(
            layout.config_file,
            PathBuf::from("/etc/oxid-relay/config.toml")
        );
        // Must equal oxid_relay_plugin::plugin_dirs() on Linux release builds.
        assert_eq!(layout.plugin_dir, PathBuf::from("/etc/oxid-relay/plugins"));
        assert_eq!(layout.data_dir, PathBuf::from("/var/lib/oxid-relay"));
        assert_eq!(
            layout.env_file,
            Some(PathBuf::from("/etc/oxid-relay/oxid-relay.env"))
        );
    }

    #[test]
    fn windows_layout_uses_given_roots() {
        let layout = InstallLayout::windows(Path::new("PF"), Path::new("PD"));
        assert_eq!(
            layout.binary,
            Path::new("PF").join("OxidRelay").join("oxid-relay.exe")
        );
        // Must equal oxid_relay_plugin::plugin_dirs() on Windows release builds.
        assert_eq!(
            layout.plugin_dir,
            Path::new("PD").join("OxidRelay").join("plugins")
        );
        assert_eq!(
            layout.config_file,
            Path::new("PD").join("OxidRelay").join("config.toml")
        );
        assert_eq!(layout.env_file, None);
    }
}
