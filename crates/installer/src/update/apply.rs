//! Swapping the installed binary for a new one, with rollback if the
//! service does not come back up.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

use crate::layout::InstallLayout;
use crate::service::ServiceManager;

/// Hint appended to permission errors.
const PRIVILEGE_HINT: &str = if cfg!(windows) {
    "run `oxid-relay update` from an elevated PowerShell (Run as administrator)"
} else {
    "run `oxid-relay update` as root (e.g. with sudo)"
};

/// Wraps an I/O error with `what`, adding a privilege hint for
/// permission errors.
pub fn io_error(err: io::Error, what: impl std::fmt::Display) -> anyhow::Error {
    if err.kind() == io::ErrorKind::PermissionDenied {
        anyhow::anyhow!("{what}: permission denied; {PRIVILEGE_HINT}")
    } else {
        anyhow::Error::new(err).context(what.to_string())
    }
}

/// Path of the backup kept next to the installed binary (`<binary>.old`).
pub fn backup_path(binary: &Path) -> PathBuf {
    let mut name = OsString::from(binary.as_os_str());
    name.push(".old");
    PathBuf::from(name)
}

/// How long to wait for the restarted service to report running.
#[derive(Debug, Clone, Copy)]
pub struct StartupCheck {
    /// Time to wait before the first check, so a binary that crashes right
    /// after starting is caught.
    pub settle: Duration,
    /// Give up if the service is not running after this long.
    pub timeout: Duration,
    /// Delay between checks.
    pub interval: Duration,
}

impl Default for StartupCheck {
    fn default() -> Self {
        Self {
            settle: Duration::from_secs(2),
            timeout: Duration::from_secs(10),
            interval: Duration::from_millis(500),
        }
    }
}

impl StartupCheck {
    /// Polls `service` until it reports running or the timeout expires.
    fn wait_running(&self, service: &dyn ServiceManager) -> anyhow::Result<bool> {
        thread::sleep(self.settle);
        let deadline = Instant::now() + self.timeout;
        loop {
            if service.is_running()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(self.interval);
        }
    }
}

/// What [`apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// Whether the service was restarted (it was running before).
    pub restarted: bool,
    /// Backup of the previous binary that is still on disk, if any.
    pub backup: Option<PathBuf>,
}

/// Replaces the installed binary with `new_binary` and runs
/// `install_extras` (plugins, example config).
///
/// The service is stopped first if it was running and restarted afterwards;
/// if it does not come back up the previous binary is restored and started
/// again, and an error is returned.
pub fn apply(
    layout: &InstallLayout,
    service: &dyn ServiceManager,
    new_binary: &Path,
    install_extras: impl FnOnce() -> anyhow::Result<()>,
    check: &StartupCheck,
) -> anyhow::Result<Applied> {
    let binary = &layout.binary;
    ensure_installed(layout)?;
    let backup = backup_path(binary);
    remove_stale_backup(&backup)?;

    let was_running = service
        .is_running()
        .context("checking whether the service is running")?;
    if was_running {
        println!("Stopping service ...");
        service.stop().context("stopping the service")?;
    }

    println!("Replacing {} ...", binary.display());
    std::fs::rename(binary, &backup)
        .map_err(|e| io_error(e, format!("backing up {}", binary.display())))?;
    if let Err(err) = install_binary(new_binary, binary).and_then(|()| install_extras()) {
        return Err(roll_back(service, binary, &backup, was_running, err));
    }

    if !was_running {
        println!("Service was not running; leaving it stopped.");
        return Ok(Applied {
            restarted: false,
            backup: finish_backup(&backup),
        });
    }

    println!("Starting service ...");
    let started = service
        .start()
        .and_then(|()| check.wait_running(service))
        .and_then(|running| {
            if running {
                Ok(())
            } else {
                bail!(
                    "the service did not report running within {:?}",
                    check.timeout
                )
            }
        });
    match started {
        Ok(()) => Ok(Applied {
            restarted: true,
            backup: finish_backup(&backup),
        }),
        Err(err) => Err(roll_back(service, binary, &backup, true, err)),
    }
}

/// Fails unless an installed binary exists.
pub fn ensure_installed(layout: &InstallLayout) -> anyhow::Result<()> {
    if !layout.binary.is_file() {
        bail!(
            "OxidRelay is not installed at {}; run `oxid-relay install` first",
            layout.binary.display()
        );
    }
    Ok(())
}

/// Removes a backup left behind by an earlier update (on Windows it may
/// still have been locked as the running executable back then).
fn remove_stale_backup(backup: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(backup) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(io_error(
            err,
            format!("removing stale backup {}", backup.display()),
        )),
        _ => Ok(()),
    }
}

/// Copies `source` to `dest` and makes it executable.
fn install_binary(source: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::copy(source, dest)
        .map_err(|e| io_error(e, format!("installing {}", dest.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| io_error(e, format!("making {} executable", dest.display())))?;
    }
    Ok(())
}

/// After a successful update: deletes the backup where possible and
/// returns it if it is still on disk. On Windows the backup is usually the
/// running executable and cannot be deleted; the next update removes it.
fn finish_backup(backup: &Path) -> Option<PathBuf> {
    if cfg!(windows) || std::fs::remove_file(backup).is_err() {
        Some(backup.to_path_buf())
    } else {
        None
    }
}

/// Restores `backup` over `binary`, restarts the service if `restart`, and
/// returns `cause` extended with what the rollback achieved.
fn roll_back(
    service: &dyn ServiceManager,
    binary: &Path,
    backup: &Path,
    restart: bool,
    cause: anyhow::Error,
) -> anyhow::Error {
    println!("Update failed, restoring the previous binary ...");
    // Best effort: the new binary may be running (and locked) or crashed.
    let _ = service.stop();
    let restored = remove_if_exists(binary)
        .and_then(|()| std::fs::rename(backup, binary))
        .map_err(|e| io_error(e, format!("restoring {}", binary.display())));
    let outcome = match restored {
        Err(err) => format!(
            "rollback FAILED ({err:#}); the previous binary is at {}",
            backup.display()
        ),
        Ok(()) if !restart => "rolled back to the previous binary".to_owned(),
        Ok(()) => match service.start() {
            Ok(()) => "rolled back to the previous binary and restarted the service".to_owned(),
            Err(err) => format!(
                "rolled back to the previous binary, but starting the service failed: {err:#}"
            ),
        },
    };
    cause.context(format!("update failed; {outcome}"))
}

/// Removes `path`, treating a missing file as success.
fn remove_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// Service manager that records calls; `start` launches whatever the
    /// installed binary says (`good` runs, anything else crashes).
    struct FakeService {
        binary: PathBuf,
        running: Cell<bool>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl FakeService {
        fn new(layout: &InstallLayout, running: bool) -> Self {
            Self {
                binary: layout.binary.clone(),
                running: Cell::new(running),
                calls: RefCell::default(),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.borrow().clone()
        }
    }

    impl ServiceManager for FakeService {
        fn register(&self, _: &InstallLayout) -> anyhow::Result<()> {
            unreachable!("update never registers")
        }
        fn unregister(&self) -> anyhow::Result<()> {
            unreachable!("update never unregisters")
        }
        fn start(&self) -> anyhow::Result<()> {
            self.calls.borrow_mut().push("start");
            let contents = std::fs::read(&self.binary)?;
            self.running.set(!contents.starts_with(b"crash"));
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<()> {
            self.calls.borrow_mut().push("stop");
            self.running.set(false);
            Ok(())
        }
        fn is_running(&self) -> anyhow::Result<bool> {
            Ok(self.running.get())
        }
        fn is_registered(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    /// A tempdir layout with an installed `old` binary and a new binary
    /// with `new_contents` next to it.
    fn setup(new_contents: &[u8]) -> (tempfile::TempDir, InstallLayout, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let mut layout = InstallLayout::linux();
        layout.bin_dir = dir.path().join("bin");
        layout.binary = layout.bin_dir.join("oxid-relay");
        layout.plugin_dir = dir.path().join("plugins");
        std::fs::create_dir_all(&layout.bin_dir).unwrap();
        std::fs::write(&layout.binary, b"old").unwrap();
        let new_binary = dir.path().join("new-oxid-relay");
        std::fs::write(&new_binary, new_contents).unwrap();
        (dir, layout, new_binary)
    }

    const FAST: StartupCheck = StartupCheck {
        settle: Duration::ZERO,
        timeout: Duration::ZERO,
        interval: Duration::ZERO,
    };

    #[test]
    fn swaps_binary_and_restarts_running_service() {
        let (_dir, layout, new_binary) = setup(b"good");
        let service = FakeService::new(&layout, true);
        let extras = Cell::new(false);
        let applied = apply(
            &layout,
            &service,
            &new_binary,
            || {
                extras.set(true);
                Ok(())
            },
            &FAST,
        )
        .unwrap();
        assert!(applied.restarted);
        assert!(extras.get());
        assert_eq!(std::fs::read(&layout.binary).unwrap(), b"good");
        assert_eq!(service.calls(), ["stop", "start"]);
        assert!(service.is_running().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&layout.binary)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
            assert_eq!(applied.backup, None);
            assert!(!backup_path(&layout.binary).exists());
        }
    }

    #[test]
    fn leaves_stopped_service_stopped() {
        let (_dir, layout, new_binary) = setup(b"good");
        let service = FakeService::new(&layout, false);
        let applied = apply(&layout, &service, &new_binary, || Ok(()), &FAST).unwrap();
        assert!(!applied.restarted);
        assert!(service.calls().is_empty());
        assert_eq!(std::fs::read(&layout.binary).unwrap(), b"good");
    }

    #[test]
    fn rolls_back_when_new_binary_does_not_start() {
        let (_dir, layout, new_binary) = setup(b"crash");
        let service = FakeService::new(&layout, true);
        let err = apply(&layout, &service, &new_binary, || Ok(()), &FAST).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("rolled back"), "{message}");
        assert!(message.contains("restarted the service"), "{message}");
        assert_eq!(std::fs::read(&layout.binary).unwrap(), b"old");
        assert!(!backup_path(&layout.binary).exists());
        assert_eq!(service.calls(), ["stop", "start", "stop", "start"]);
        assert!(service.is_running().unwrap());
    }

    #[test]
    fn rolls_back_when_extras_fail() {
        let (_dir, layout, new_binary) = setup(b"good");
        let service = FakeService::new(&layout, true);
        let err = apply(
            &layout,
            &service,
            &new_binary,
            || bail!("plugin copy failed"),
            &FAST,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("plugin copy failed"));
        assert_eq!(std::fs::read(&layout.binary).unwrap(), b"old");
        assert!(service.is_running().unwrap());
    }

    #[test]
    fn removes_stale_backup_first() {
        let (_dir, layout, new_binary) = setup(b"good");
        std::fs::write(backup_path(&layout.binary), b"stale").unwrap();
        let service = FakeService::new(&layout, false);
        apply(&layout, &service, &new_binary, || Ok(()), &FAST).unwrap();
        assert_eq!(std::fs::read(&layout.binary).unwrap(), b"good");
    }

    #[test]
    fn requires_existing_installation() {
        let (_dir, layout, new_binary) = setup(b"good");
        std::fs::remove_file(&layout.binary).unwrap();
        let service = FakeService::new(&layout, false);
        let err = apply(&layout, &service, &new_binary, || Ok(()), &FAST).unwrap_err();
        assert!(err.to_string().contains("oxid-relay install"), "{err}");
        assert!(service.calls().is_empty());
    }

    #[test]
    fn permission_errors_mention_privileges() {
        let err = io_error(
            io::Error::from(io::ErrorKind::PermissionDenied),
            "writing x",
        );
        assert!(err.to_string().contains("permission denied"), "{err}");
        assert!(err.to_string().contains(PRIVILEGE_HINT), "{err}");
        let err = io_error(io::Error::from(io::ErrorKind::NotFound), "writing x");
        assert!(!err.to_string().contains(PRIVILEGE_HINT));
    }

    #[test]
    fn backup_path_appends_suffix() {
        assert_eq!(
            backup_path(Path::new("bin/oxid-relay.exe")),
            PathBuf::from("bin/oxid-relay.exe.old")
        );
    }
}
