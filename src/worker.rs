//! Inner worker session: spawn `claude -p`, stream + format its events, run a
//! heartbeat and a watchdog, detect rate-limits. Port of a prior bespoke harness's
//! worker block — but Rust gives us the child PID and threads directly, so the
//! watchdog is simpler and race-free.

use crate::config::AggConfig;
use crate::state::{ActivityEvent, LiveState};
use crate::stream::{self, ActivityTracker};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct SessionOutcome {
    pub exit_code: Option<i32>,
    pub duration_secs: u64,
    pub rate_limited: bool,
    /// the loop doesn't branch on this yet, but it's part of the outcome surface
    #[allow(dead_code)]
    pub killed_by_watchdog: bool,
    /// output-side tokens this session reported on its result event (for the budget)
    pub output_tokens: u64,
    /// the worker's `💬` thoughts this session (raw material for the LLM summarizer)
    pub thoughts: Vec<String>,
    /// this session's claude session_id (for optional `--resume` continuity)
    pub session_id: Option<String>,
}

/// Run one worker session to completion and return its outcome. If `resume_id` is
/// Some, the worker continues that prior session's context (`--resume`) instead of
/// a fresh context — opt-in (see `resume_sessions` config; default fresh).
pub fn run_session(
    cfg: &AggConfig,
    prompt: &str,
    dir: &std::path::Path,
    session: u32,
    resume_id: Option<&str>,
    live: &LiveState,
) -> SessionOutcome {
    let start = Instant::now();

    let mut command = Command::new("claude");
    command
        .arg("--dangerously-skip-permissions")
        .arg("--model")
        .arg(&cfg.model)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose");
    if let Some(id) = resume_id {
        command.arg("--resume").arg(id);
    }
    // Own process group (pgid == pid) so the watchdog can SIGKILL the WHOLE tree —
    // the worker AND every tool subprocess it spawned. A bare kill(pid) leaves
    // orphan grandchildren (a runaway build/sleep) running, which is exactly the
    // hang the watchdog exists to stop.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command
        .arg("-p")
        .arg(prompt)
        .current_dir(dir)
        .stdin(Stdio::null()) // </dev/null — never block on a TTY read
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  FAILED to spawn claude worker: {e}");
            return SessionOutcome {
                exit_code: None,
                duration_secs: 0,
                rate_limited: false,
                killed_by_watchdog: false,
                output_tokens: 0,
                thoughts: vec![],
                session_id: None,
            };
        }
    };

    let pid = child.id();
    // shared state between reader thread, heartbeat thread, and watchdog thread
    let last_activity = Arc::new(AtomicU64::new(now_epoch())); // epoch secs of last stream event
    let last_thought = Arc::new(std::sync::Mutex::new(String::from("session start")));
    let rate_limited = Arc::new(AtomicBool::new(false));
    let output_tokens = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let killed = Arc::new(AtomicBool::new(false));

    // ---- reader thread: format the stream, update activity + rate-limit + tokens ----
    // It sends its result on a channel (not via join) so the main thread can collect
    // it with a TIMEOUT — if a grandchild keeps the stdout pipe open, the reader may
    // block on EOF forever, and we must not let that wedge the loop.
    let stdout = child.stdout.take().expect("piped stdout");
    let (reader_tx, reader_rx) = std::sync::mpsc::channel::<(Vec<String>, Option<String>)>();
    {
        let last_activity = last_activity.clone();
        let last_thought = last_thought.clone();
        let rate_limited = rate_limited.clone();
        let output_tokens = output_tokens.clone();
        let live = live.clone();
        // throttle disk writes of the live stream so we don't rewrite state.json on
        // every token. The in-memory snapshot still updates on every event; only the
        // atomic file write (and seq bump) is rate-limited to ~once a second.
        let throttle = Duration::from_millis(800);
        std::thread::spawn(move || {
            let mut tracker = ActivityTracker::default();
            let mut session_id: Option<String> = None;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(ev) = stream::format_event(&line) {
                    // print to the live log (the plain stdout stream — source of truth)
                    println!("{} | {}", hhmmss(), ev.display);
                    if !ev.is_result {
                        last_activity.store(now_epoch(), Ordering::Relaxed);
                    }
                    if let Some(thought) = &ev.thought {
                        *last_thought.lock().unwrap() = thought.clone();
                    }
                    // push the event into the shared dashboard state so the TUI's
                    // Activity tail reflects the foreground stream in REAL TIME (the
                    // bug this fixes: now/think/recent were empty mid-session).
                    let act = ActivityEvent { ts: hhmmss(), kind: ev.kind.tag().to_string(), text: ev.text.clone() };
                    live.update_throttled(throttle, |s| {
                        s.idle_secs = 0;
                        s.push_event(act);
                    });
                    tracker.observe(&ev);
                }
                // capture the session_id (for optional --resume continuity)
                if let Some(id) = stream::session_id_from_result(&line) {
                    session_id = Some(id);
                }
                // rate-limit detection: only the terminal result event matters
                if stream::line_is_rate_limited_result(&line) {
                    rate_limited.store(true, Ordering::Relaxed);
                }
                // accumulate output tokens reported on the result event (budget)
                let toks = stream::output_tokens_from_result(&line);
                if toks > 0 {
                    output_tokens.fetch_add(toks, Ordering::Relaxed);
                }
            }
            let _ = reader_tx.send((tracker.thoughts, session_id)); // ok if receiver timed out
        });
    };

    // ---- heartbeat thread ----
    let heartbeat = {
        let last_activity = last_activity.clone();
        let last_thought = last_thought.clone();
        let done = done.clone();
        let interval = cfg.heartbeat_secs;
        let live = live.clone();
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                sleep_secs(interval, &done);
                if done.load(Ordering::Relaxed) {
                    break;
                }
                let idle = now_epoch().saturating_sub(last_activity.load(Ordering::Relaxed));
                let up = start.elapsed().as_secs();
                let thought = last_thought.lock().unwrap().clone();
                let flag = if idle >= 240 { format!(" ⚠IDLE{idle}s") } else { String::new() };
                eprintln!(
                    "[HB {}] sess#{session} +{}m{:02}s | idle {idle}s{flag} | now: {}",
                    hhmmss(),
                    up / 60,
                    up % 60,
                    truncate(&thought, 140)
                );
                // keep the dashboard's idle indicator live between stream events — a
                // quiet worker (long tool, deep think) still ticks idle upward here.
                live.update(|s| s.idle_secs = idle);
            }
        })
    };

    // ---- watchdog thread: kill a hung worker (stream-idle AND cpu-flat) ----
    let watchdog = {
        let last_activity = last_activity.clone();
        let done = done.clone();
        let killed = killed.clone();
        let idle_thresh = cfg.watchdog.idle_secs;
        let cpu_grace = cfg.watchdog.cpu_grace;
        std::thread::spawn(move || {
            // single-threaded state (only this thread touches it). `last_cpu` is reset
            // to -1 whenever we leave an idle window, so each window starts a fresh
            // two-sample comparison rather than comparing against an ancient sample.
            let mut last_cpu: i64 = -1;
            let mut cpu_flat_since: Option<u64> = None;
            while !done.load(Ordering::Relaxed) {
                sleep_secs(30, &done);
                if done.load(Ordering::Relaxed) {
                    break;
                }
                let idle = now_epoch().saturating_sub(last_activity.load(Ordering::Relaxed));
                if idle < idle_thresh {
                    cpu_flat_since = None;
                    last_cpu = -1; // worker active → restart flat-detection cleanly
                    continue;
                }
                // stream-idle long enough — now require CPU to be flat too
                let cpu = cpu_jiffies(pid);
                let prev = std::mem::replace(&mut last_cpu, cpu);
                if cpu >= 0 && cpu == prev {
                    let since = *cpu_flat_since.get_or_insert(now_epoch());
                    let flat = now_epoch().saturating_sub(since);
                    if flat >= cpu_grace {
                        // re-check `done` right before killing: the main thread may have
                        // reaped the child in the up-to-30s gap, and the pid could be reused.
                        if done.load(Ordering::Relaxed) {
                            break;
                        }
                        eprintln!(
                            "⚠ WATCHDOG: worker pid={pid} hung (stream-idle {idle}s + cpu-flat {flat}s) — SIGKILL"
                        );
                        kill(pid);
                        killed.store(true, Ordering::Relaxed);
                        break;
                    }
                } else {
                    cpu_flat_since = None; // CPU advanced => real work, reset (last_cpu holds fresh sample)
                }
            }
        })
    };

    // ---- wait for the worker, then tear down the helper threads ----
    let status = child.wait().ok();
    done.store(true, Ordering::Relaxed);

    // The immediate worker has exited. If it spawned a tool subprocess that inherited
    // our stdout pipe and is still alive (orphaned), the reader thread would block on
    // the pipe forever and hang `run_session`. Kill the whole process group to release
    // any lingering pipe holder, so the reader sees EOF promptly. (No-op if the group
    // is already gone.) Then collect the reader with a BOUNDED join so a stuck pipe can
    // never wedge the loop — the exact production-hang class this harness guards against.
    //
    // EXCEPTION: if a registered `agg spawn` long task is still inside the worker's group
    // (it didn't detach into its own group), a blind group SIGKILL would kill legitimate
    // background work. Spare protected pgids — a properly-spawned task has its OWN group
    // and its own log (not the worker pipe), so this only matters for the edge case, and
    // there the per-pid protected sweep below releases any real pipe-holder anyway.
    let protected = crate::spawns::protected_pgids(dir);
    if protected.is_empty() {
        kill(pid);
    } else {
        // protected tasks present: do a protected-aware sweep instead of a blind group kill.
        let _ = crate::reap::reap_pgid_except(pid, &protected);
    }
    // bounded collect: if the reader is still blocked on a held-open pipe after the
    // group kill, give up after 10s with whatever we have (the thread dies on its own
    // when the pipe finally closes; it's detached).
    let (thoughts, session_id) = reader_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_default();
    let _ = heartbeat.join();
    let _ = watchdog.join();

    // Final straggler sweep: the worker is its own process-group leader, so its pid IS the
    // group id. Reap any process still in that group — orphaned `nohup`/`--watch`/detached
    // grandchildren that survived the one-shot `kill(pid)` above. Env-free + cross-platform
    // (pgid is queryable everywhere, unlike a hardened process's environment on macOS).
    //
    // BUT spare registered `agg spawn` long tasks: a worker may have deliberately left a
    // multi-hour sim running to poll next session. Their pgids are PROTECTED so the sweep
    // kills real leaks but not intentional background work. (`protected` computed above.)
    let reaped = crate::reap::reap_pgid_except(pid, &protected);
    if reaped > 0 {
        eprintln!("  reaped {reaped} straggler process(es) from the worker group");
    }

    SessionOutcome {
        exit_code: status.and_then(|s| s.code()),
        duration_secs: start.elapsed().as_secs(),
        // a clean exit 0 is never a rate-limit (the prior harness GATE 1)
        rate_limited: rate_limited.load(Ordering::Relaxed)
            && status.and_then(|s| s.code()).unwrap_or(0) != 0,
        killed_by_watchdog: killed.load(Ordering::Relaxed),
        output_tokens: output_tokens.load(Ordering::Relaxed),
        thoughts,
        session_id,
    }
}

// ---------------- small platform helpers ----------------

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hhmmss() -> String {
    // local HH:MM:SS via the cached libc offset (no chrono dependency) so the
    // dashboard's Activity tail matches the user's wall clock, not UTC.
    crate::localtime::hhmmss(now_epoch())
}

/// Sleep up to `secs`, waking early in 1s steps if `done` flips.
fn sleep_secs(secs: u64, done: &AtomicBool) {
    for _ in 0..secs {
        if done.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Cumulative CPU time (jiffies/ticks) of a pid, or -1 if unavailable. Used only
/// to detect "CPU not advancing" — absolute units don't matter, only equality.
fn cpu_jiffies(pid: u32) -> i64 {
    // `ps -o time=` gives [[DD-]HH:]MM:SS — parse to total seconds. Portable on
    // macOS + Linux without a dependency. (1s resolution is plenty for a 180s grace.)
    let out = Command::new("ps").args(["-o", "time=", "-p", &pid.to_string()]).output();
    let Ok(out) = out else { return -1 };
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    if s.is_empty() {
        return -1;
    }
    parse_ps_time(s).unwrap_or(-1)
}

/// Parse a `ps` TIME field like `MM:SS`, `HH:MM:SS`, or `DD-HH:MM:SS` to seconds.
/// Returns None on ANY malformed field — never a fake 0, which could read as
/// "CPU flat" and contribute to a false-positive watchdog kill.
fn parse_ps_time(s: &str) -> Option<i64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<i64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<i64> = rest.split(':').map(|p| p.parse::<i64>()).collect::<Result<_, _>>().ok()?;
    let hms = match parts.as_slice() {
        [h, m, sec] => h * 3600 + m * 60 + sec,
        [m, sec] => m * 60 + sec,
        [sec] => *sec,
        _ => return None,
    };
    Some(days * 86400 + hms)
}

#[cfg(unix)]
fn kill(pid: u32) {
    // SIGKILL the worker's whole PROCESS GROUP (negative pid). The worker is its own
    // group leader (process_group(0) at spawn), so this reaps it AND every tool
    // subprocess it spawned — a bare kill(pid) would orphan grandchildren.
    unsafe {
        libc_kill(-(pid as i32), 9);
    }
}
#[cfg(not(unix))]
fn kill(pid: u32) {
    // /T kills the whole process tree on Windows.
    let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).output();
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::parse_ps_time;
    #[test]
    fn ps_time_formats() {
        assert_eq!(parse_ps_time("0:08"), Some(8));
        assert_eq!(parse_ps_time("8:29"), Some(8 * 60 + 29));
        assert_eq!(parse_ps_time("1:02:03"), Some(3723));
        assert_eq!(parse_ps_time("2-01:00:00"), Some(2 * 86400 + 3600));
        // malformed → None (not a fake 0)
        assert_eq!(parse_ps_time("garbage"), None);
        assert_eq!(parse_ps_time("1:2:3:4"), None);
        assert_eq!(parse_ps_time(""), None);
    }
}
