//! An unpacked release bundle and copying its files into an installation.
//!
//! Shared by `install` (from the directory the running binary lives in) and
//! `update` (from a freshly extracted archive).

use std::path::{Path, PathBuf};

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
        todo!("installer agent: validate {root:?} contains the binary and plugins/")
    }

    /// The bundle the running binary was started from.
    pub fn from_current_exe() -> anyhow::Result<Self> {
        todo!("installer agent")
    }

    /// Path of the binary inside the bundle.
    pub fn binary(&self) -> PathBuf {
        self.root.join(BINARY_NAME)
    }

    /// Copies bundled plugins (overwriting plugins of the same name, leaving
    /// other plugins alone), `config.example.toml` and, per `options`, the
    /// binary and a seeded `config.toml` into `layout`.
    pub fn copy_into(&self, layout: &InstallLayout, options: CopyOptions) -> anyhow::Result<()> {
        todo!(
            "installer agent: copy {:?} into {layout:?} with {options:?}",
            self.root
        )
    }
}
