//! `agg run --report-json` — the headless contract: logs go to `agg/private/run.log`, stdout carries
//! the report and nothing else.
//!
//! The redirect is done at the FILE-DESCRIPTOR level, not by threading a writer through the code:
//! log lines come from the loop, from the worker's stream-reader thread, and from every child agg
//! spawns (judges, hooks, the agent CLI's own stderr). Pointing fd 1 and fd 2 at the log catches all
//! of them, including output agg does not know it produces. The ORIGINAL descriptors are kept, so the
//! report still reaches the caller's stdout and a fatal reason still reaches its stderr.

use std::io::Write as _;
use std::path::Path;

/// The caller's original stdout/stderr, held while fd 1/2 point at the run log.
pub struct Headless {
    #[cfg(unix)]
    stdout: std::os::fd::OwnedFd,
    #[cfg(unix)]
    stderr: std::os::fd::OwnedFd,
}

impl Headless {
    /// Send this process's stdout and stderr to `agg/private/run.log` (appending), keeping the
    /// originals for [`Headless::emit_report`] / [`Headless::emit_error`].
    #[cfg(unix)]
    pub fn redirect(dir: &Path) -> std::io::Result<Headless> {
        use nix::unistd::{dup, dup2_stderr, dup2_stdout};
        std::fs::create_dir_all(crate::paths::private_dir(dir))?;
        let log = std::fs::OpenOptions::new().create(true).append(true).open(crate::paths::run_log(dir))?;
        // flush what Rust buffered for the ORIGINAL stdout before it stops being fd 1.
        let _ = std::io::stdout().flush();
        let stdout = dup(std::io::stdout())?;
        let stderr = dup(std::io::stderr())?;
        dup2_stdout(&log)?;
        dup2_stderr(&log)?;
        Ok(Headless { stdout, stderr })
    }

    /// The printer without the redirect — for a run with no project directory to log into, which
    /// still owes its caller a report on stdout.
    #[cfg(unix)]
    pub fn redirect_none() -> std::io::Result<Headless> {
        Ok(Headless { stdout: nix::unistd::dup(std::io::stdout())?, stderr: nix::unistd::dup(std::io::stderr())? })
    }

    /// No fd juggling off unix: the report is printed, the logs stay where they are.
    #[cfg(not(unix))]
    pub fn redirect(_dir: &Path) -> std::io::Result<Headless> {
        Ok(Headless {})
    }

    #[cfg(not(unix))]
    pub fn redirect_none() -> std::io::Result<Headless> {
        Ok(Headless {})
    }

    /// Write the report to the caller's stdout — the ONLY thing that ever goes there.
    pub fn emit_report(&self, text: &str) -> std::io::Result<()> {
        let _ = std::io::stdout().flush();
        let mut out = self.original(true)?;
        out.write_all(text.trim_end().as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()
    }

    /// A fatal reason for the caller's stderr (it is also in the log): a CI job whose report could
    /// not be written must still say why without anyone opening a file.
    pub fn emit_error(&self, line: &str) {
        if let Ok(mut e) = self.original(false) {
            let _ = writeln!(e, "{line}");
        }
    }

    #[cfg(unix)]
    fn original(&self, stdout: bool) -> std::io::Result<std::fs::File> {
        let fd = if stdout { &self.stdout } else { &self.stderr };
        Ok(std::fs::File::from(fd.try_clone()?))
    }

    #[cfg(not(unix))]
    fn original(&self, stdout: bool) -> std::io::Result<Box<dyn std::io::Write>> {
        Ok(if stdout { Box::new(std::io::stdout()) } else { Box::new(std::io::stderr()) })
    }
}

/// Settle a finished `agg run`: report any error, make sure a report exists, print it for
/// `--report-json`, and return the exit code — which is the report's.
///
/// `may_write` is false when another run is live in the project (its report file is not ours).
/// A run that left no report of its own — an error before the loop began, or a report that could
/// not be written — gets a fallback `error` report here, and exit 1.
pub fn conclude(
    dir: &Path,
    project: &str,
    res: anyhow::Result<crate::plugin::RunOutcome>,
    may_write: bool,
    out: Option<&Headless>,
) -> u8 {
    let mut code = res.as_ref().map(|o| o.exit_code()).unwrap_or(1);
    let say = |line: &str| {
        eprintln!("{line}");
        if let Some(h) = out {
            h.emit_error(line);
        }
    };
    if let Err(e) = &res {
        say(&format!("error: {e:#}"));
    }
    let existing = if may_write { super::read_text(dir) } else { None };
    let text = match existing {
        Some(t) => t,
        None => {
            let msg = match &res {
                Err(e) => format!("{e:#}"),
                // the loop returns an Err when its report write fails, so an Ok with no report means
                // the file vanished underneath it: the run's ending is unknown, which is an error.
                Ok(_) => "the run ended but left no report".to_string(),
            };
            code = 1;
            let (rep, written) = super::write_fallback(dir, project, &msg, may_write);
            if let Err(e) = written {
                say(&format!("error: cannot write {}: {e}", crate::paths::report_json(dir).display()));
            }
            serde_json::to_string_pretty(&rep).unwrap_or_default()
        }
    };
    if let Some(h) = out {
        if let Err(e) = h.emit_report(&text) {
            say(&format!("error: cannot print the report: {e}"));
            code = 1;
        }
    }
    code
}
