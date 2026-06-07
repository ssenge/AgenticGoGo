//! Long-task registry — the bookkeeping for processes a worker leaves running past its turn.
//!
//! THE PROBLEM. A `claude -p` worker exits when its turn ends. If it launches a long
//! simulation (minutes-to-hours) and exits intending to poll the result in a *later*
//! session, two things go wrong today:
//!   1. `run_session` reaps the worker's whole process group on exit (the straggler
//!      sweep) — killing the legitimate long task along with any real leak. The reaper
//!      can't tell "intentional background work" from "leaked grandchild".
//!   2. If the task escaped the worker's pgid (a `setsid`/double-fork detach), it
//!      survives as a `ppid=1` ORPHAN that no session knows about — so the next session
//!      relaunches a duplicate, piling up runaway CPU.
//!
//! THE FIX (three cooperating layers; this module is layer 2 + the engine for layer 3):
//!   • Layer 1 — guidance (in AGG_RESUME): don't end your turn while a spawned task runs;
//!     poll-and-resume until it finishes. If this held 100% the problem never occurs — but
//!     context-fill / rate-limit / watchdog can end a session anyway, so it can't be relied on.
//!   • Layer 2 — `agg spawn --reason "…" -- <cmd>`: agg launches the task itself, so it
//!     KNOWS the pgid directly (no env marker — macOS blocks reading a process's env). It
//!     records {pgid, reason, log, started_session, status} here. That record (a) marks the
//!     pgid PROTECTED so the post-session reaper SPARES it, and (b) is injected into the next
//!     session so the worker POLLS instead of relaunching.
//!   • Layer 3 — a scanner that runs every session boundary: reap registered tasks whose
//!     owning loop is gone (status done/dead → group killed), and detect-but-only-report
//!     anything suspicious it can't prove is ours. AUTONOMOUS-SAFE: it only KILLS a pgid it
//!     recorded as ours; never kills on suspicion (there is no human to ask).
//!
//! Why pgid is the ownership handle (not ppid, not an env marker): a backgrounded child
//! KEEPS its pgid after its parent dies and re-parents to pid 1 — verified — whereas its
//! `ppid` flips to 1 (lineage lost). And modern macOS blocks reading another process's
//! environment, so an `AGG_RUN_ID` env tag is unreadable from outside. pgid, recorded by
//! agg at spawn time while it still owns the process, is the stable, provable, cross-platform
//! "this is ours" key.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One registered long-running task.
///
/// `#[serde(default)]` for forward/backward compat: a spawns.json written by a different
/// `agg` version still deserializes (same discipline as state.json).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SpawnEntry {
    /// short operator/worker-chosen handle, e.g. "scaling-20q". Unique within a project.
    pub name: String,
    /// process-group id of the spawned task. THE ownership + reaping key (stable across
    /// the parent dying; agg set it via `process_group(0)` at spawn).
    pub pgid: u32,
    /// the task's own pid (== pgid, since it leads its group). Kept for liveness checks.
    pub pid: u32,
    /// WHY this was started — mandatory at spawn time, surfaced to the next worker so it
    /// knows what is pending and does not relaunch a duplicate.
    pub reason: String,
    /// the command line, for the operator and the next worker to recognize the task.
    pub cmd: String,
    /// path to the task's combined stdout+stderr log (under `.agg/spawns/<name>.log`).
    pub log: String,
    /// the session number that launched it (for the "started session N" breadcrumb).
    pub started_session: u32,
    /// "running" | "done" | "reaped". A finished/own-loop-gone task moves off "running".
    pub status: String,
}

/// The on-disk registry: a list of entries, persisted atomically like state.json.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Registry {
    pub spawns: Vec<SpawnEntry>,
}

impl Registry {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(".agg").join("spawns.json")
    }

    /// Read the registry, or an empty one if missing/unparseable (never fails the loop).
    pub fn load(dir: &Path) -> Registry {
        std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Write atomically (tmp + rename) so a concurrent reader never sees a torn file.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let dest = Self::path(dir);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = dest.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &dest)
    }

    /// Add (or replace by name) a spawn entry and persist.
    pub fn register(dir: &Path, entry: SpawnEntry) -> std::io::Result<()> {
        let mut reg = Self::load(dir);
        reg.spawns.retain(|e| e.name != entry.name);
        reg.spawns.push(entry);
        reg.save(dir)
    }

    /// Directory holding per-spawn logs.
    pub fn log_dir(dir: &Path) -> PathBuf {
        dir.join(".agg").join("spawns")
    }
}

/// The set of pgids the post-session reaper must SPARE: every entry still marked "running"
/// whose process is actually alive. Anything else (done, or recorded-but-dead) is fair game.
pub fn protected_pgids(dir: &Path) -> HashSet<u32> {
    Registry::load(dir)
        .spawns
        .iter()
        .filter(|e| e.status == "running" && pid_alive(e.pid))
        .map(|e| e.pgid)
        .collect()
}

/// Layer-3 scanner, run at each session boundary. For every registered entry:
///   • still "running" and alive  → leave it (it's doing intentional work, protected).
///   • marked "running" but its process is GONE → it finished; flip to "done" so the next
///     session reads a completed task and consumes the result instead of relaunching.
/// Returns a short human line describing what changed, if anything (for the loop log).
///
/// NOTE on autonomous safety: this NEVER kills a process it didn't record. It only updates
/// status for tasks it owns. Active reaping of our-but-stale groups happens through the
/// existing `reap_pgid` path on stop; here we only bookkeep liveness so the cross-session
/// handoff stays accurate. A future tick could group-kill our recorded-but-orphaned pgids,
/// but it must match on a recorded pgid — never on a guess.
pub fn scan(dir: &Path) -> Option<String> {
    let mut reg = Registry::load(dir);
    if reg.spawns.is_empty() {
        return None;
    }
    let mut transitioned = Vec::new();
    for e in reg.spawns.iter_mut() {
        if e.status == "running" && !pid_alive(e.pid) {
            e.status = "done".into();
            transitioned.push(e.name.clone());
        }
    }
    // garbage-collect long-finished entries so the file doesn't grow unbounded: keep
    // running tasks and recently-done ones; drop entries that are done AND whose log is gone.
    let before = reg.spawns.len();
    reg.spawns.retain(|e| e.status == "running" || Path::new(&e.log).exists());
    let pruned = before - reg.spawns.len();

    if transitioned.is_empty() && pruned == 0 {
        return None;
    }
    let _ = reg.save(dir);
    let mut msg = String::new();
    if !transitioned.is_empty() {
        msg.push_str(&format!("spawn task(s) finished: {}", transitioned.join(", ")));
    }
    if pruned > 0 {
        if !msg.is_empty() {
            msg.push_str("; ");
        }
        msg.push_str(&format!("pruned {pruned} stale entr(y/ies)"));
    }
    Some(msg)
}

/// A compact, prompt-ready summary of currently-tracked tasks, injected into the next
/// worker session so it knows what is pending (and WHY) and does not relaunch duplicates.
/// Returns None when nothing is registered (no noise added to the prompt).
pub fn summary_for_prompt(dir: &Path) -> Option<String> {
    let reg = Registry::load(dir);
    let live: Vec<&SpawnEntry> = reg.spawns.iter().filter(|e| e.status == "running").collect();
    let done: Vec<&SpawnEntry> = reg.spawns.iter().filter(|e| e.status == "done").collect();
    if live.is_empty() && done.is_empty() {
        return None;
    }
    let mut s = String::from(
        "═══ BACKGROUND TASKS (agg spawn) — read before launching anything ═══\n",
    );
    for e in &live {
        s.push_str(&format!(
            "• RUNNING  `{}` (pid {}, since session {}) — {}\n    cmd: {}\n    log: {}\n    → DO NOT relaunch it. Poll the log; if it is still running, finish your other work and EXIT (a later session re-polls). If it just finished, consume its result.\n",
            e.name, e.pid, e.started_session, e.reason, e.cmd, e.log
        ));
    }
    for e in &done {
        s.push_str(&format!(
            "• DONE     `{}` — {} (consume its result: {})\n",
            e.name, e.reason, e.log
        ));
    }
    Some(s)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe { libc_kill(pid as i32, 0) == 0 }
}
#[cfg(not(unix))]
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let mut d = std::env::temp_dir();
        // unique-ish without Date/rand: use the test pid + a static counter
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        d.push(format!("agg-spawns-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(d.join(".agg")).unwrap();
        d
    }

    #[test]
    fn register_then_load_roundtrips() {
        let d = tmpdir();
        Registry::register(&d, SpawnEntry {
            name: "t1".into(), pgid: 12345, pid: 12345, reason: "test run".into(),
            cmd: "sleep 1".into(), log: "/tmp/x.log".into(), started_session: 3, status: "running".into(),
        }).unwrap();
        let reg = Registry::load(&d);
        assert_eq!(reg.spawns.len(), 1);
        assert_eq!(reg.spawns[0].name, "t1");
        assert_eq!(reg.spawns[0].reason, "test run");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn register_replaces_same_name() {
        let d = tmpdir();
        for r in ["first", "second"] {
            Registry::register(&d, SpawnEntry {
                name: "dup".into(), pgid: 1, pid: 1, reason: r.into(),
                cmd: "x".into(), log: "/tmp/x.log".into(), started_session: 1, status: "running".into(),
            }).unwrap();
        }
        let reg = Registry::load(&d);
        assert_eq!(reg.spawns.len(), 1, "same name should replace, not duplicate");
        assert_eq!(reg.spawns[0].reason, "second");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn protected_pgids_excludes_dead_and_done() {
        let d = tmpdir();
        // a "running" entry with a dead pid (pid 0 is never alive) must NOT be protected.
        Registry::register(&d, SpawnEntry {
            name: "dead".into(), pgid: 999, pid: 0, reason: "x".into(),
            cmd: "x".into(), log: "/tmp/x.log".into(), started_session: 1, status: "running".into(),
        }).unwrap();
        assert!(protected_pgids(&d).is_empty(), "dead pid must not be protected");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn scan_flips_running_to_done_when_process_gone() {
        let d = tmpdir();
        // pid 0 is never alive → scan should flip it to done. Give it an existing log so it's
        // not pruned in the same pass (we want to observe the transition).
        let log = d.join("x.log");
        std::fs::write(&log, "").unwrap();
        Registry::register(&d, SpawnEntry {
            name: "gone".into(), pgid: 1, pid: 0, reason: "x".into(),
            cmd: "x".into(), log: log.to_string_lossy().into(), started_session: 1, status: "running".into(),
        }).unwrap();
        let msg = scan(&d);
        assert!(msg.is_some(), "scan should report the transition");
        let reg = Registry::load(&d);
        assert_eq!(reg.spawns[0].status, "done");
        std::fs::remove_dir_all(&d).ok();
    }
}
