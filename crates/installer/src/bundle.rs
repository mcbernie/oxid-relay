//! An unpacked release bundle and copying its files into an installation.
//!
//! Shared by `install` (from the directory the running binary lives in) and
//! `update` (from a freshly extracted archive).

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::layout::InstallLayout;

/// File name of the binary inside a bundle.
pub const BINARY_NAME: &str = if cfg!(windows) {
    "oxid-relay.exe"
} else {
    "oxid-relay"
};

/// A directory laid out like a release archive.
#[derive(Debug, Clone)]
pub struct Bundle {
    /// Root directory of the bundle.
    pub root: PathBuf,
}

/// What to copy from a bundle.
#[derive(Debug, Clone, Copy, Default)]
pub struct CopyOptions {
    /// Copy the binary. `update` swaps it separately (with backup/rollback).
    pub binary: bool,
    /// Create `config.toml` from `config.example.toml` if none exists yet.
    /// An existing configuration is never overwritten.
    pub seed_config: bool,
}

impl Bundle {
    /// Wraps a directory, checking that it looks like a bundle.
    pub fn from_dir(root: &Path) -> anyhow::Result<Self> {
        let bundle = Self {
            root: root.to_path_buf(),
        };
        if !bundle.binary().is_file() {
            anyhow::bail!(
                "{} is not a release bundle: {BINARY_NAME} is missing",
                root.display()
            );
        }
        if !bundle.plugins_dir().is_dir() {
            anyhow::bail!(
                "{} is not a release bundle: the plugins/ directory is missing \
                 (run this from the unpacked release archive)",
                root.display()
            );
        }
        Ok(bundle)
    }

    /// The bundle the running binary was started from.
    pub fn from_current_exe() -> anyhow::Result<Self> {
        let exe = std::env::current_exe().context("locating the running binary")?;
        let root = exe
            .parent()
            .with_context(|| format!("{} has no parent directory", exe.display()))?;
        Self::from_dir(root)
    }

    /// Path of the binary inside the bundle.
    pub fn binary(&self) -> PathBuf {
        self.root.join(BINARY_NAME)
    }

    /// Path of the bundled plugin directory.
    pub fn plugins_dir(&self) -> PathBuf {
        self.root.join("plugins")
    }

    /// Path of the bundled example configuration.
    pub fn example_config(&self) -> PathBuf {
        self.root.join(EXAMPLE_CONFIG)
    }

    /// Copies bundled plugins (overwriting plugins of the same name, leaving
    /// other plugins alone), `config.example.toml` and, per `options`, the
    /// binary and a seeded `config.toml` into `layout`.
    pub fn copy_into(&self, layout: &InstallLayout, options: CopyOptions) -> anyhow::Result<()> {
        for dir in [&layout.config_dir, &layout.plugin_dir, &layout.data_dir] {
            create_dir(dir)?;
        }

        self.copy_plugins(&layout.plugin_dir)?;

        let example = self.example_config();
        if example.is_file() {
            copy_file(&example, &layout.config_dir.join(EXAMPLE_CONFIG))?;
            if options.seed_config && !layout.config_file.exists() {
                copy_file(&example, &layout.config_file)?;
            }
        }

        if options.binary {
            create_dir(&layout.bin_dir)?;
            install_binary(&self.binary(), &layout.binary)?;
        }
        Ok(())
    }

    /// Replaces `dest/<name>` with each bundled `plugins/<name>` directory.
    fn copy_plugins(&self, dest: &Path) -> anyhow::Result<()> {
        let source = self.plugins_dir();
        if same_file(&source, dest) {
            return Ok(());
        }
        for entry in read_dir(&source)? {
            let entry = entry.with_context(|| format!("reading {}", source.display()))?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let target = dest.join(entry.file_name());
            if target.exists() {
                std::fs::remove_dir_all(&target)
                    .with_context(|| format!("removing old plugin {}", target.display()))?;
            }
            copy_dir_all(&entry.path(), &target)?;
        }
        Ok(())
    }
}

/// File name of the example configuration inside a bundle and next to the
/// installed `config.toml`.
pub const EXAMPLE_CONFIG: &str = "config.example.toml";

/// Whether two paths name the same existing file or directory.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn create_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
}

fn read_dir(dir: &Path) -> anyhow::Result<std::fs::ReadDir> {
    std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))
}

fn copy_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::copy(from, to)
        .map(drop)
        .with_context(|| format!("copying {} to {}", from.display(), to.display()))
}

/// Recursively copies the directory `from` to `to` (which must not exist).
fn copy_dir_all(from: &Path, to: &Path) -> anyhow::Result<()> {
    create_dir(to)?;
    for entry in read_dir(from)? {
        let entry = entry.with_context(|| format!("reading {}", from.display()))?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Copies the binary next to `dest` and renames it into place, so a binary
/// that is still mapped by a process is replaced rather than overwritten.
/// Skips the copy when `source` already is `dest` (running the installed
/// binary, which Windows could not overwrite anyway).
fn install_binary(source: &Path, dest: &Path) -> anyhow::Result<()> {
    if same_file(source, dest) {
        return Ok(());
    }
    let file_name = dest
        .file_name()
        .with_context(|| format!("{} has no file name", dest.display()))?;
    let mut staged_name = std::ffi::OsString::from(".");
    staged_name.push(file_name);
    staged_name.push(".new");
    let staged = dest.with_file_name(staged_name);

    copy_file(source, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("making {} executable", staged.display()))?;
    }
    std::fs::rename(&staged, dest).with_context(|| {
        let _ = std::fs::remove_file(&staged);
        format!("moving the new binary to {}", dest.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates `path` (and parents) with `contents`.
    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    /// A bundle with one plugin `teams` and an example config.
    fn make_bundle(root: &Path) -> Bundle {
        write(&root.join(BINARY_NAME), "new binary");
        write(&root.join("plugins/teams/plugin.toml"), "new manifest");
        write(&root.join("plugins/teams/lib/main.rhai"), "new script");
        write(&root.join("plugins/README.txt"), "not a plugin");
        write(&root.join(EXAMPLE_CONFIG), "example");
        Bundle::from_dir(root).unwrap()
    }

    fn make_layout(root: &Path) -> InstallLayout {
        InstallLayout {
            bin_dir: root.join("bin"),
            binary: root.join("bin").join(BINARY_NAME),
            config_dir: root.join("etc"),
            config_file: root.join("etc/config.toml"),
            plugin_dir: root.join("etc/plugins"),
            data_dir: root.join("data"),
            env_file: None,
        }
    }

    #[test]
    fn from_dir_requires_binary_and_plugins() {
        let dir = tempfile::tempdir().unwrap();
        let err = Bundle::from_dir(dir.path()).unwrap_err().to_string();
        assert!(err.contains(BINARY_NAME), "{err}");

        write(&dir.path().join(BINARY_NAME), "");
        let err = Bundle::from_dir(dir.path()).unwrap_err().to_string();
        assert!(err.contains("plugins/"), "{err}");

        std::fs::create_dir(dir.path().join("plugins")).unwrap();
        assert_eq!(Bundle::from_dir(dir.path()).unwrap().root, dir.path());
    }

    #[test]
    fn fresh_install_copies_everything() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let bundle = make_bundle(src.path());
        let layout = make_layout(dst.path());

        let options = CopyOptions {
            binary: true,
            seed_config: true,
        };
        bundle.copy_into(&layout, options).unwrap();

        assert_eq!(read(&layout.binary), "new binary");
        assert_eq!(read(&layout.config_file), "example");
        assert_eq!(read(&layout.config_dir.join(EXAMPLE_CONFIG)), "example");
        assert_eq!(
            read(&layout.plugin_dir.join("teams/lib/main.rhai")),
            "new script"
        );
        assert!(!layout.plugin_dir.join("README.txt").exists());
        assert!(layout.data_dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&layout.binary)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn reinstall_keeps_config_and_foreign_plugins() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let bundle = make_bundle(src.path());
        let layout = make_layout(dst.path());
        write(&layout.config_file, "my config");
        write(&layout.plugin_dir.join("teams/stale.rhai"), "old file");
        write(&layout.plugin_dir.join("custom/plugin.toml"), "mine");
        write(&layout.binary, "old binary");

        let options = CopyOptions {
            binary: false,
            seed_config: true,
        };
        bundle.copy_into(&layout, options).unwrap();

        assert_eq!(read(&layout.config_file), "my config");
        assert_eq!(read(&layout.plugin_dir.join("custom/plugin.toml")), "mine");
        assert_eq!(
            read(&layout.plugin_dir.join("teams/plugin.toml")),
            "new manifest"
        );
        assert!(!layout.plugin_dir.join("teams/stale.rhai").exists());
        assert_eq!(read(&layout.binary), "old binary");
    }

    #[test]
    fn no_seed_leaves_config_missing() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let layout = make_layout(dst.path());
        make_bundle(src.path())
            .copy_into(&layout, CopyOptions::default())
            .unwrap();
        assert!(!layout.config_file.exists());
        assert!(layout.config_dir.join(EXAMPLE_CONFIG).exists());
    }

    #[test]
    fn copying_bundle_onto_itself_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = make_bundle(dir.path());
        let layout = InstallLayout {
            bin_dir: dir.path().to_path_buf(),
            binary: bundle.binary(),
            plugin_dir: bundle.plugins_dir(),
            ..make_layout(dir.path())
        };
        let options = CopyOptions {
            binary: true,
            seed_config: false,
        };
        bundle.copy_into(&layout, options).unwrap();
        assert_eq!(read(&bundle.binary()), "new binary");
        assert_eq!(
            read(&bundle.plugins_dir().join("teams/plugin.toml")),
            "new manifest"
        );
    }
}
