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

use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Command, Stdio};

/// The pidfile/logfile live under `.agg/` next to `state.json` and `bus/`.
fn pid_path(dir: &Path) -> std::path::PathBuf {
    dir.join(".agg").join("run.pid")
}

/// Record THIS process as the live loop. Called by every `agg run` (foreground OR detached)
/// so `agg stop` and the double-run guard always read a current pid — previously only the
/// `--detach` path wrote run.pid, so a foreground / `nohup agg run` left it pointing at a
/// stale (often dead) pid, and `agg stop` would target the wrong process.
pub fn write_run_pid(dir: &Path) {
    let _ = std::fs::create_dir_all(dir.join(".agg"));
    let _ = std::fs::write(pid_path(dir), std::process::id().to_string());
}

/// Remove run.pid (best-effort) when the loop exits, so a later `agg stop` doesn't act on a
/// dead pid and the double-run guard doesn't falsely report a live loop.
pub fn clear_run_pid(dir: &Path) {
    let _ = std::fs::remove_file(pid_path(dir));
}
fn log_path(dir: &Path) -> std::path::PathBuf {
    dir.join(".agg").join("run.log")
}

/// Spawn the loop detached and return immediately. Refuses if a previous detached run
/// is still alive (a stale pidfile from a dead process is cleaned up and ignored).
pub fn spawn_detached(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir.join(".agg")).with_context(|| "creating .agg dir")?;

    // Guard against a double-detach: if run.pid points at a live process, bail.
    if let Some(pid) = live_pid(dir) {
        anyhow::bail!(
            "a detached loop is already running here (pid {pid}).\n  \
             follow it:  tail -f {}\n  \
             stop it:    agg stop",
            log_path(dir).display()
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
        .open(log_path(dir))
        .with_context(|| format!("opening {}", log_path(dir).display()))?;
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
        unsafe {
            cmd.pre_exec(|| {
                // new session → no controlling tty → survives terminal close.
                if libc_setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = cmd.spawn().with_context(|| "spawning detached loop")?;
    let pid = child.id();
    std::fs::write(pid_path(dir), pid.to_string())
        .with_context(|| format!("writing {}", pid_path(dir).display()))?;

    eprintln!(
        "▶ detached loop started (pid {pid}).\n  \
         follow:    tail -f {log}\n  \
         dashboard: agg dashboard\n  \
         stop:      agg stop",
        log = log_path(dir).display(),
    );
    Ok(())
}

/// Read `.agg/run.pid` and return the PID iff that process is still alive. A stale
/// pidfile (process gone) is removed and `None` returned.
fn live_pid(dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(pid_path(dir)).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    if process_alive(pid) {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(pid_path(dir)); // clean up the stale file
        None
    }
}

/// True if `pid` names a live process. `kill(pid, 0)` probes existence without signaling.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    unsafe { libc_kill(pid as i32, 0) == 0 }
}
#[cfg(not(unix))]
fn process_alive(pid: u32) -> bool {
    // Windows: ask tasklist whether the PID exists.
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(unix)]
extern "C" {
    #[link_name = "setsid"]
    fn libc_setsid() -> i32;
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
