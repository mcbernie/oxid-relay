//! Checks that the installer runs with administrator rights.

/// Fails with a clear message unless the process may install services:
/// root on Unix, an elevated (administrator) process on Windows.
pub fn ensure_admin() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } != 0 {
            anyhow::bail!("root privileges required: run this command with sudo");
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        super::windows::ensure_elevated()
    }
}
