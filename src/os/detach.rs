//! `agg run --detach` — run the loop in the background.
//!
//! Long loops want to outlive the terminal. Rather than make the user remember
//! `nohup agg run > .agg/run.log 2>&1 &`, `--detach` does it for them: it re-execs
//! `agg run` (with the SAME args, minus the detach flag) as a child in its own
//! session, redirects the child's stdout+stderr to `.agg/run.log`, writes the child
//! PID to `.agg/run.pid`, prints how to follow/stop, and returns.
//!
//! The child is an ordinary foreground `agg run` from its own point of view — all the
//! loop/worker/watchdog machinery is unchanged; only its stdio and session differ.

use crate::paths::{agg_dir, run_log, run_pid};
use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Stdio};

/// Record THIS process as the live loop. Called by every `agg run` (foreground OR detached)
/// so `agg stop` and the double-run guard always read a current pid — previously only the
/// `--detach` path wrote run.pid, so a foreground / `nohup agg run` left it pointing at a
/// stale (often dead) pid, and `agg stop` would target the wrong process.
pub fn write_run_pid(dir: &Path) {
    let _ = std::fs::create_dir_all(agg_dir(dir));
    let _ = std::fs::write(run_pid(dir), std::process::id().to_string());
}

/// Remove run.pid (best-effort) when the loop exits, so a later `agg stop` doesn't act on a
/// dead pid and the double-run guard doesn't falsely report a live loop.
pub fn clear_run_pid(dir: &Path) {
    let _ = std::fs::remove_file(run_pid(dir));
}

/// Spawn the loop detached and return immediately. Refuses if a previous detached run
/// is still alive (a stale pidfile from a dead process is cleaned up and ignored).
pub fn spawn_detached(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(agg_dir(dir)).with_context(|| "creating .agg dir")?;

    // Guard against a double-detach: if run.pid points at a live process, bail.
    if let Some(pid) = live_pid(dir) {
        anyhow::bail!(
            "a detached loop is already running here (pid {pid}).\n  \
             follow it:  tail -f {}\n  \
             stop it:    agg stop",
            run_log(dir).display()
        );
    }

    // Reconstruct the child's argv from our own, dropping the detach flag so the child
    // runs in the foreground (of its own detached session). argv[0] is replaced with the
    // resolved current-exe path so a PATH-relative invocation still works after detach.
    let exe = std::env::current_exe().with_context(|| "resolving current executable")?;
    let child_args: Vec<String> = std::env::args()
        .skip(1) // drop argv[0]
        .filter(|a| a != "--detach" && a != "-d")
        .collect();

    // Append to the log so successive detached runs accumulate (with a separator).
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_log(dir))
        .with_context(|| format!("opening {}", run_log(dir).display()))?;
    let log_err = log.try_clone().with_context(|| "duplicating log handle")?;

    let mut cmd = Command::new(&exe);
    cmd.args(&child_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

    // Detach from the controlling terminal: a new session (setsid) so the child isn't
    // killed when the launching shell exits or receives SIGHUP. `process_group(0)` makes
    // the child its own group leader; `pre_exec(setsid)` fully divorces the session.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // new session → no controlling tty → survives terminal close.
        unsafe {
            cmd.pre_exec(crate::os::proc::setsid);
        }
    }

    let child = cmd.spawn().with_context(|| "spawning detached loop")?;
    let pid = child.id();
    std::fs::write(run_pid(dir), pid.to_string())
        .with_context(|| format!("writing {}", run_pid(dir).display()))?;

    eprintln!(
        "▶ detached loop started (pid {pid}).\n  \
         follow:    tail -f {log}\n  \
         dashboard: agg dashboard\n  \
         stop:      agg stop",
        log = run_log(dir).display(),
    );
    Ok(())
}

/// Read `.agg/run.pid` and return the PID iff that process is still alive. A stale
/// pidfile (process gone) is removed and `None` returned. Public so the loop can use it as a
/// double-run guard on BOTH the foreground and the detached path.
pub fn live_pid(dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(run_pid(dir)).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    if crate::os::proc::pid_alive(pid) {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(run_pid(dir)); // clean up the stale file
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "agg-detach-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(agg_dir(&d)).unwrap();
        d
    }

    #[test]
    fn live_pid_none_when_no_pidfile() {
        let d = tmpdir("nopid");
        assert_eq!(live_pid(&d), None);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn live_pid_reports_our_own_live_process() {
        let d = tmpdir("self");
        write_run_pid(&d); // writes OUR pid, which is alive
        assert_eq!(live_pid(&d), Some(std::process::id()));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn live_pid_cleans_up_a_stale_pidfile() {
        let d = tmpdir("stale");
        // pid 0 is never a live process → live_pid must return None AND delete the file,
        // so the double-run guard treats a crashed loop's leftover as "no loop running".
        std::fs::write(run_pid(&d), "0").unwrap();
        assert_eq!(live_pid(&d), None);
        assert!(!run_pid(&d).exists(), "stale pidfile should be removed");
        std::fs::remove_dir_all(&d).ok();
    }
}
