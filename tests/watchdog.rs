//! The test suite's own watchdog: a spawned run ends at its bound with its whole process group, and a helper that
//! gives up ends the test binary failed instead of leaving the run under test waiting for ever.

mod common;

use common::Bounded;
use std::process::Command;
use std::time::{Duration, Instant};

/// Run only by `a_helper_that_gives_up_ends_the_test_binary_failed`, in a child process.
#[test]
#[ignore]
fn the_helper_that_gives_up() {
    common::give_up("nothing appeared");
}

/// A helper that gives up ends the whole test binary with exit 101 (a panic in its thread would end nothing).
#[test]
fn a_helper_that_gives_up_ends_the_test_binary_failed() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "the_helper_that_gives_up", "--nocapture"])
        .output_for(60)
        .unwrap();
    assert_eq!(out.status.code(), Some(101), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("GIVING UP"), "it says who gave up");
}

/// A run that never ends is killed at its bound, and so is what it started: the bound fails the test within about
/// five seconds.
#[test]
fn a_run_that_never_ends_is_killed_at_its_bound_with_its_children() {
    let tmp = tempfile::tempdir().unwrap();
    let pidfile = tmp.path().join("child.pid");
    let started = Instant::now();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Command::new("sh")
            .arg("-c")
            .arg(format!("sleep 600 & echo $! > {}; wait", pidfile.display()))
            .output_for(3)
    }));
    assert!(caught.is_err(), "the bound fails the run");
    assert!(started.elapsed() < Duration::from_secs(30), "it ended near its bound");
    let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
    assert!(!alive, "the child the run started is gone too");
}
