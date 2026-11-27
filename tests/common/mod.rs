//! Shared helpers for the integration tests: spawns that cannot outlive a bound, and waits for an ask that end
//! the whole test binary instead of leaving the run under test waiting for ever.
#![allow(dead_code)] // each test crate uses a part

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The longest a spawned run of the binary may take.
pub const SPAWN_SECS: u64 = 900;
/// How long a helper waits for an ask before it gives up.
pub const ASK_PATIENCE: Duration = Duration::from_secs(600);

/// Ends the test binary, failed: a helper thread's panic would leave the run under test waiting for ever (one test
/// hung for nine hours on 2026-11-24).
pub fn give_up(what: &str) -> ! {
    let thread = std::thread::current();
    eprintln!("GIVING UP in `{}`: {what}", thread.name().unwrap_or("a helper thread"));
    std::process::exit(101)
}

/// The first open ask, waiting up to `ASK_PATIENCE` for it; ends the test binary when none appears.
pub fn wait_open_ask(dir: &Path) -> agg::core::asks::Ask {
    let end = Instant::now() + ASK_PATIENCE;
    while Instant::now() < end {
        if let Some(a) = agg::core::asks::open(dir).into_iter().next() {
            return a;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    give_up("no ask appeared")
}

/// `output()` with a bound: the child runs in its own process group, which is killed when the bound passes.
pub trait Bounded {
    /// Runs the command to its end, at most `secs` seconds; panics with what it said when the bound passes.
    fn output_for(&mut self, secs: u64) -> std::io::Result<Output>;
    /// `output_for` with the default bound.
    fn output_bounded(&mut self) -> std::io::Result<Output> {
        self.output_for(SPAWN_SECS)
    }
}

impl Bounded for Command {
    fn output_for(&mut self, secs: u64) -> std::io::Result<Output> {
        use std::os::unix::process::CommandExt;
        let child = self
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let pgid = nix::unistd::Pid::from_raw(child.id() as i32);
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let dog = std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < end {
                if flag.load(Ordering::SeqCst) {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
            true
        });
        let out = child.wait_with_output()?;
        finished.store(true, Ordering::SeqCst);
        if dog.join().unwrap() {
            panic!(
                "the run was killed after {secs} s\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(out)
    }
}
