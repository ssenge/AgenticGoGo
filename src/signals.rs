//! SIGINT / SIGTERM handling for `agg run` (unix).
//!
//! Without this, a terminal Ctrl-C on `agg run` (a) orphans the live `claude` worker — it runs
//! in its OWN process group (`process_group(0)` in worker.rs), so the terminal's SIGINT never
//! reaches it — and (b) skips every Drop-based cleanup (run.pid, on_stop hooks, the run ledger),
//! because a default-disposition signal death does not unwind the stack.
//!
//! The fix, dependency-free and async-signal-safe:
//!   • a handler for SIGINT/SIGTERM that does only two signal-safe things — `kill(-pgid, SIGKILL)`
//!     the currently-registered worker group, and set an `interrupted` flag;
//!   • the loop registers the worker's pgid while a session runs and clears it after. The
//!     group-kill makes the worker's blocking `child.wait()` return, so control comes back to the
//!     loop, which sees `interrupted()` at the next phase boundary and returns normally — running
//!     all the Drop guards.
//!
//! Windows has no POSIX process groups and Ctrl-C already terminates the whole console group, so
//! this is a no-op there.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// pgid of the worker whose session is currently running (0 = none). The signal handler kills
/// this group so a Ctrl-C doesn't orphan the worker.
static WORKER_PGID: AtomicU32 = AtomicU32::new(0);
/// set once a SIGINT/SIGTERM has been received; the loop checks it at phase boundaries.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
    #[link_name = "signal"]
    fn libc_signal(signum: i32, handler: usize) -> usize;
}

#[cfg(unix)]
const SIGINT: i32 = 2;
#[cfg(unix)]
const SIGTERM: i32 = 15;
#[cfg(unix)]
const SIGKILL: i32 = 9;

/// The C signal handler. MUST stay async-signal-safe: only an atomic store and a `kill(2)`.
#[cfg(unix)]
extern "C" fn on_signal(_sig: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
    let pgid = WORKER_PGID.load(Ordering::SeqCst);
    if pgid != 0 {
        // SIGKILL the worker's whole process group so no tool subprocess is orphaned, and so the
        // loop's blocking child.wait() returns and control comes back for graceful cleanup.
        unsafe {
            libc_kill(-(pgid as i32), SIGKILL);
        }
    }
    // Do NOT exit(2) here — returning lets the loop unwind through its Drop guards (run.pid,
    // on_stop hooks, ledger). The handler stays installed, so a second Ctrl-C is idempotent
    // (re-kills the — now dead — group and re-sets the flag); the loop exits at the next boundary.
}

/// Install the SIGINT/SIGTERM handlers. Call once at the start of `agg run`. No-op on non-unix.
pub fn install() {
    #[cfg(unix)]
    unsafe {
        let handler = on_signal as *const () as usize;
        libc_signal(SIGINT, handler);
        libc_signal(SIGTERM, handler);
    }
}

/// Register the pgid of the worker whose session is starting (so a signal kills its group).
pub fn set_worker_pgid(pgid: u32) {
    WORKER_PGID.store(pgid, Ordering::SeqCst);
}

/// Clear the registered worker pgid (the session ended). A signal after this only sets the flag.
pub fn clear_worker_pgid() {
    WORKER_PGID.store(0, Ordering::SeqCst);
}

/// Has a SIGINT/SIGTERM been received? The loop checks this at phase boundaries and, if true,
/// returns so its Drop guards (run.pid, on_stop hooks, ledger) run.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_and_pgid_roundtrip() {
        // default state
        clear_worker_pgid();
        assert_eq!(WORKER_PGID.load(Ordering::SeqCst), 0);
        set_worker_pgid(4242);
        assert_eq!(WORKER_PGID.load(Ordering::SeqCst), 4242);
        clear_worker_pgid();
        assert_eq!(WORKER_PGID.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn install_is_idempotent_and_safe() {
        // installing twice must not panic (handlers just get re-set).
        install();
        install();
    }
}
