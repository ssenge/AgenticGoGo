//! SIGINT / SIGTERM handling for `agg run` (unix).
//!
//! Without this, a terminal Ctrl-C on `agg run` (a) orphans the live `claude` worker — it runs
//! in its OWN process group (`process_group(0)` in worker.rs), so the terminal's SIGINT never
//! reaches it — and (b) skips every Drop-based cleanup (run.pid, on_stop hooks, the run ledger),
//! because a default-disposition signal death does not unwind the stack.
//!
//! The fix, in two halves:
//!   • on SIGINT/SIGTERM we `killpg(worker, SIGKILL)` the currently-registered worker group and
//!     set an `interrupted` flag;
//!   • the loop registers the worker's pgid while a session runs and clears it after. The
//!     group-kill makes the worker's blocking `child.wait()` return, so control comes back to the
//!     loop, which sees `interrupted()` at the next phase boundary and returns normally — running
//!     all the Drop guards.
//!
//! We deliberately do NOT exit(2) on the signal: returning is what lets the loop unwind through
//! its Drop guards. The hook stays installed, so a second Ctrl-C is idempotent (it re-kills the —
//! now dead — group and re-sets the flag) and the loop exits at the next boundary.
//!
//! This used to be a hand-rolled `signal(2)` FFI block with an `extern "C"` handler, which meant
//! every line of the handler had to be async-signal-safe by hand. `signal_hook::iterator::Signals`
//! moves the work off the handler entirely: its handler only writes a byte to a self-pipe, and we
//! do the flag-set + group-kill on an ordinary background thread, in ordinary Rust, with no
//! `unsafe` and no async-signal-safety obligation. The added latency is one pipe wakeup.
//!
//! Windows has no POSIX process groups and Ctrl-C already terminates the whole console group, so
//! this is a no-op there.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// pgid of the worker whose session is currently running (0 = none). The signal thread kills
/// this group so a Ctrl-C doesn't orphan the worker.
static WORKER_PGID: AtomicU32 = AtomicU32::new(0);
/// set once a SIGINT/SIGTERM has been received; the loop checks it at phase boundaries.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Install the SIGINT/SIGTERM handling. Call once at the start of `agg run`; safe to call again
/// (the `Once` makes repeat calls no-ops rather than stacking a second listener thread).
/// No-op on non-unix.
pub fn install() {
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let Ok(mut signals) = Signals::new([SIGINT, SIGTERM]) else {
                // registration failed — leave the default disposition. A Ctrl-C then kills us
                // without cleanup, which is the pre-signals behaviour, not a new failure mode.
                return;
            };
            std::thread::spawn(move || {
                for _ in signals.forever() {
                    INTERRUPTED.store(true, Ordering::SeqCst);
                    let pgid = WORKER_PGID.load(Ordering::SeqCst);
                    if pgid != 0 {
                        // SIGKILL the worker's whole group so no tool subprocess is orphaned, and
                        // so the loop's blocking child.wait() returns for graceful cleanup.
                        crate::os::proc::kill_group(pgid);
                    }
                }
            });
        });
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
        // installing twice must not panic, and must not stack a second listener thread.
        install();
        install();
    }

    // The end-to-end contract — a real SIGINT to a real `agg run` sets the flag, kills the worker
    // GROUP, and unwinds through the Drop guards — is covered by
    // `tests/cli.rs::interrupt_during_run_skips_verify_and_the_exit_log`, which signals an actual
    // process. It is deliberately not unit-tested here: raising SIGINT inside the test binary
    // would flip the INTERRUPTED global for every other test in the process.
}
