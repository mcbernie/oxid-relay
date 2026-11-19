//! A tiny abstraction over running external commands, so the sequence of
//! `systemctl`/`useradd` calls can be unit-tested with a recorder.

use std::process::Command;

use anyhow::Context as _;

/// Result of a finished command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    /// Whether the command exited with status 0.
    pub success: bool,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Runs external programs.
pub trait CommandRunner {
    /// Runs `program` with `args` and waits for it. An `Err` means the
    /// program could not be started at all; a non-zero exit is reported via
    /// [`CommandOutput::success`].
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput>;
}

/// Runs commands for real via [`std::process::Command`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        let output = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("running `{program}` (is it installed and on PATH?)"))?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Runs a command and turns a non-zero exit into an error carrying the
/// command line and its stderr.
pub fn run_checked(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[&str],
) -> anyhow::Result<CommandOutput> {
    let output = runner.run(program, args)?;
    if output.success {
        Ok(output)
    } else {
        let detail = output.stderr.trim();
        let detail = if detail.is_empty() {
            output.stdout.trim()
        } else {
            detail
        };
        anyhow::bail!("`{program} {}` failed: {detail}", args.join(" "))
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A recording [`CommandRunner`] for tests.

    use std::cell::RefCell;

    use super::{CommandOutput, CommandRunner};

    /// Records every command line and answers with a scripted exit status.
    pub struct FakeRunner {
        /// Command lines run so far, e.g. `"systemctl enable oxid-relay"`.
        pub calls: RefCell<Vec<String>>,
        /// Decides whether a command line succeeds.
        succeeds: Box<dyn Fn(&str) -> bool>,
    }

    impl FakeRunner {
        /// A runner where every command succeeds.
        pub fn ok() -> Self {
            Self::with(|_| true)
        }

        /// A runner where `succeeds(command_line)` decides the exit status.
        pub fn with(succeeds: impl Fn(&str) -> bool + 'static) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                succeeds: Box::new(succeeds),
            }
        }

        /// The recorded command lines.
        pub fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
            let line = std::iter::once(program)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            let success = (self.succeeds)(&line);
            self.calls.borrow_mut().push(line);
            Ok(CommandOutput {
                success,
                stdout: String::new(),
                stderr: if success {
                    String::new()
                } else {
                    "boom".into()
                },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeRunner;
    use super::*;

    #[test]
    fn run_checked_reports_stderr() {
        let runner = FakeRunner::with(|_| false);
        let err = run_checked(&runner, "systemctl", &["start", "x"]).unwrap_err();
        assert_eq!(err.to_string(), "`systemctl start x` failed: boom");
    }

    #[test]
    fn run_checked_passes_success_through() {
        let runner = FakeRunner::ok();
        assert!(run_checked(&runner, "true", &[]).unwrap().success);
        assert_eq!(runner.calls(), vec!["true"]);
    }
}
