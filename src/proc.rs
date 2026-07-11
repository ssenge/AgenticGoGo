//! Process primitives — the ONE home for the OS-level operations the harness leans on.
//!
//! Before this module these were copy-pasted across judge/summary/worker/reap/spawns/detach
//! (the `kill(2)` FFI block alone lived in six files, and two of the copies had already begun
//! to diverge). Process killing, liveness, and the timeout-runner are correctness-sensitive;
//! they belong in exactly one place. Everything cross-platform lives here behind a `#[cfg]`.
//!
//! The unix syscalls come from `nix`'s safe wrappers rather than a hand-declared `extern "C"`
//! block — same syscalls, no `unsafe`, and it typechecks the pid/signal arguments that a raw
//! `kill(-pgid, 9)` leaves as bare integers.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use nix::{
    sys::signal::{kill, killpg, Signal},
    unistd::Pid,
};

// ---------------- liveness + single-pid / group kills ----------------

/// Is `pid` alive? (`kill(pid, 0)` on unix; `tasklist` on Windows.) A pid of 0 is never alive.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // signal `None` is the POSIX `sig == 0` existence probe: permission-checked, sends nothing.
        kill(Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(not(unix))]
    {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

/// SIGKILL a single pid. Returns whether the signal was delivered. On Windows this is a
/// tree-kill (`taskkill /T`) — there is no single-process-without-children primitive there.
pub fn kill_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        kill(Pid::from_raw(pid as i32), Signal::SIGKILL).is_ok()
    }
    #[cfg(not(unix))]
    {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// SIGKILL an entire process group at once (`killpg(2)` on unix). On Windows (no POSIX groups)
/// `pgid` carries the leader pid and `taskkill /T` tree-kills it.
///
/// Async-signal-safe on unix (a bare `killpg(2)`), which is what lets `signals.rs` call this
/// from its SIGINT/SIGTERM path to stop the worker group without orphaning it.
pub fn kill_group(pgid: u32) {
    #[cfg(unix)]
    {
        let _ = killpg(Pid::from_raw(pgid as i32), Signal::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pgid.to_string(), "/T", "/F"])
            .output();
    }
}

/// The pgid of a single process, or None if it's gone / not determinable. Used by the reaper
/// to honour the protected-spawn set per-pid. Windows has no POSIX groups → always None.
pub fn group_of(pid: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        // `getpgid(2)` — was a `ps -o pgid= -p <pid>` fork+exec per call, and the reaper calls it
        // once per pid in the group.
        nix::unistd::getpgid(Some(Pid::from_raw(pid as i32)))
            .ok()
            .map(|pg| pg.as_raw() as u32)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// Every pid currently in process group `pgid`. Unix lists via `ps`; Windows has no POSIX
/// groups (tree-kill on the leader pid does the real reaping) → returns empty.
pub fn pids_in_group(pgid: u32) -> Vec<u32> {
    #[cfg(unix)]
    {
        let mut out = Vec::new();
        let Ok(o) = Command::new("ps").args(["-A", "-o", "pid=,pgid="]).output() else {
            return out;
        };
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let mut it = line.split_whitespace();
            let (Some(pid), Some(pg)) = (it.next(), it.next()) else { continue };
            if let (Ok(pid), Ok(pg)) = (pid.parse::<u32>(), pg.parse::<u32>()) {
                if pg == pgid {
                    out.push(pid);
                }
            }
        }
        out
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        Vec::new()
    }
}

/// `setsid(2)` — start a new session + process group with the caller as leader, dropping the
/// controlling tty. Call from a `pre_exec` closure (which is itself `unsafe`: only
/// async-signal-safe work is permitted in the post-fork/pre-exec window, and `setsid` qualifies).
/// Returns Err if the syscall failed, so the caller can abort the spawn rather than launch a
/// child still tied to the parent's group.
#[cfg(unix)]
pub fn setsid() -> std::io::Result<()> {
    nix::unistd::setsid().map(|_| ()).map_err(std::io::Error::from)
}

// ---------------- timeout-aware command runner ----------------

/// Captured output of a finished command.
#[derive(Debug)]
pub struct Captured {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// did the command exit 0?
    pub success: bool,
}

/// Spawn `command` with piped stdout/stderr and a wall-clock timeout. Returns `Err(reason)`
/// on spawn failure or timeout (the child's whole group is SIGKILLed first).
///
/// Both pipes are drained on background threads WHILE we wait, so a command that emits more
/// than the OS pipe buffer (~64 KB) can't block on write and get false-killed at the deadline.
/// On unix the child runs in its own process group so the timeout kill reaps grandchildren too.
///
/// The caller sets `current_dir`/env on `command` before calling (this keeps the runner
/// policy-free: the judge runs in the project dir, the summarizer wherever it likes).
pub fn run_with_timeout(mut command: Command, timeout_secs: u64) -> Result<Captured, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0); // own group so we can kill the whole tree on timeout
    }
    let mut child = command.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();

    // drain both pipes concurrently so the child never blocks on a full pipe
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = std::io::Read::read_to_end(p, &mut buf);
        }
        buf
    });
    let err_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = std::io::Read::read_to_end(p, &mut buf);
        }
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let success;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                success = status.success();
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_group(pid);
                    let _ = child.wait();
                    return Err(format!("timed out after {timeout_secs}s"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("wait: {e}")),
        }
    }
    // child exited; the drain threads finish as the pipes hit EOF
    let stdout = out_h.join().unwrap_or_default();
    let stderr = err_h.join().unwrap_or_default();
    Ok(Captured { stdout, stderr, success })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_zero_is_never_alive() {
        assert!(!pid_alive(0));
    }

    #[test]
    fn current_process_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn run_with_timeout_captures_stdout_and_success() {
        let mut c = Command::new("sh");
        c.arg("-c").arg("printf hello; exit 0");
        let out = run_with_timeout(c, 10).expect("should complete");
        assert_eq!(out.stdout, b"hello");
        assert!(out.success);
    }

    #[test]
    fn run_with_timeout_reports_nonzero_exit() {
        let mut c = Command::new("sh");
        c.arg("-c").arg("exit 3");
        let out = run_with_timeout(c, 10).expect("should complete");
        assert!(!out.success);
    }

    #[test]
    fn run_with_timeout_times_out_and_kills() {
        let mut c = Command::new("sh");
        c.arg("-c").arg("sleep 5");
        let err = run_with_timeout(c, 1).unwrap_err();
        assert!(err.contains("timed out"), "got: {err}");
    }

    #[test]
    fn run_with_timeout_survives_large_output() {
        // ~200 KB before exit — well past the ~64 KB pipe buffer. Without concurrent draining
        // this would deadlock and false-timeout.
        let mut c = Command::new("sh");
        c.arg("-c").arg("for i in $(seq 1 4000); do echo 'padding padding padding padding'; done");
        let out = run_with_timeout(c, 15).expect("should not false-timeout");
        assert!(out.success);
        assert!(out.stdout.len() > 100_000);
    }
}
