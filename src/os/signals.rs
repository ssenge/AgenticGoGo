//! SIGINT / SIGTERM handling for `rlt run` (unix).
//!
//! Without this, a terminal Ctrl-C on `rlt run` (a) orphans the live `claude` worker — it runs
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// pgids of the workers whose sessions are running now (several when lanes run at once). The signal thread kills
/// every one of these groups so a Ctrl-C doesn't orphan a worker.
static WORKERS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// set once a SIGINT/SIGTERM has been received; the loop checks it at phase boundaries.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Install the SIGINT/SIGTERM handling. Call once at the start of `rlt run`; safe to call again
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
                    // SIGKILL each worker's whole group so no tool subprocess is orphaned, and so the loop's
                    // blocking child.wait() returns for graceful cleanup.
                    for pgid in workers() {
                        crate::os::proc::kill_group(pgid);
                    }
                }
            });
        });
    }
}

/// The registered worker groups now (what a signal kills; Windows has no groups to kill, only the tests read it there).
#[cfg_attr(not(unix), allow(dead_code))]
fn workers() -> Vec<u32> {
    WORKERS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A worker's registered process group: a signal kills it while this lives; dropping it (the session ended)
/// removes that group only, never another lane's.
#[must_use = "the group is registered only while the guard lives"]
pub struct WorkerGroup(u32);

impl Drop for WorkerGroup {
    fn drop(&mut self) {
        let mut all = WORKERS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = all.iter().position(|p| *p == self.0) {
            all.remove(at);
        }
    }
}

/// Register the pgid of the worker whose session is starting (so a signal kills its group).
pub fn register_worker_pgid(pgid: u32) -> WorkerGroup {
    WORKERS.lock().unwrap_or_else(|e| e.into_inner()).push(pgid);
    WorkerGroup(pgid)
}

/// Has a SIGINT/SIGTERM been received? The loop checks this at phase boundaries and, if true,
/// returns so its Drop guards (run.pid, on_stop hooks, ledger) run.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two workers at once are both registered; one ending removes only its own group.
    #[test]
    fn several_worker_groups_register_and_leave_one_by_one() {
        let (a, b) = (register_worker_pgid(4242), register_worker_pgid(4243));
        assert!(workers().contains(&4242) && workers().contains(&4243));
        drop(a);
        assert!(!workers().contains(&4242) && workers().contains(&4243), "a's end must not clear b");
        drop(b);
        assert!(!workers().contains(&4243));
    }

    /// A signal kills every registered group: real child processes in their own groups, both gone after one kill pass.
    #[cfg(unix)]
    #[test]
    fn a_kill_pass_ends_every_registered_worker() {
        use std::os::unix::process::CommandExt;
        let spawn = || std::process::Command::new("sleep").arg("30").process_group(0).spawn().unwrap();
        let (mut one, mut two) = (spawn(), spawn());
        let _guards = [register_worker_pgid(one.id()), register_worker_pgid(two.id())];
        for pgid in workers().into_iter().filter(|p| *p == one.id() || *p == two.id()) {
            crate::os::proc::kill_group(pgid);
        }
        assert!(!one.wait().unwrap().success() && !two.wait().unwrap().success());
    }

    #[test]
    fn install_is_idempotent_and_safe() {
        // installing twice must not panic, and must not stack a second listener thread.
        install();
        install();
    }

    // The end-to-end contract — a real SIGINT to a real `rlt run` sets the flag, kills the worker
    // GROUP, and unwinds through the Drop guards — is covered by
    // `tests/cli.rs::interrupt_during_run_skips_verify_and_the_exit_log`, which signals an actual
    // process. It is deliberately not unit-tested here: raising SIGINT inside the test binary
    // would flip the INTERRUPTED global for every other test in the process.
}
