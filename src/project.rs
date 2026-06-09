//! Persistent per-project state: `.agg/project.json`.
//!
//! Each `agg run` is otherwise amnesiac — the per-run session counter resets to
//! 0, so a restart looks like the work started over even when a project has run
//! for hours across many invocations. This module gives a project a durable
//! identity and a **run-history ledger**: every `agg run` appends a record at
//! launch and finalizes it on exit (sessions, tokens, wall-time, why it ended).
//!
//! Derived facts (lifetime session total, lifetime tokens) are computed from the
//! ledger rather than stored separately, so there is a single source of truth.
//!
//! Robustness: all reads tolerate a missing/older/corrupt file (a fresh or
//! pre-this-version project just starts with an empty ledger) and all writes are
//! best-effort — the ledger is an operator-facing record, never load-bearing for
//! solve correctness, so a write failure must never kill the loop.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One completed (or in-flight) `agg run` invocation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunRecord {
    /// monotonic index within this project's history (1-based).
    pub run: u32,
    /// epoch secs the run started.
    pub started_at_epoch: u64,
    /// epoch secs the run ended (0 while still in-flight).
    pub ended_at_epoch: u64,
    /// pid of the loop process (so a crashed run's record is still attributable).
    pub pid: u32,
    /// sessions executed in THIS run.
    pub sessions: u32,
    /// output tokens spent in THIS run.
    pub tokens: u64,
    /// goals met / total at the moment the run ended.
    pub goals_met: usize,
    pub goals_total: usize,
    /// how the run ended: "goals-met" | "halt:<reason>" | "stopped" |
    /// "max-sessions" | "error" | "crashed" (in-flight record never finalized).
    pub end_reason: String,
}

/// The whole `.agg/project.json` document: stable identity + the run ledger.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    /// stable project name (from agg.yaml; informational).
    pub name: String,
    /// epoch secs the project's history began (first run we ever recorded).
    pub created_at_epoch: u64,
    /// every run, oldest first.
    pub runs: Vec<RunRecord>,
}

impl Project {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(".agg").join("project.json")
    }

    /// Load the ledger, or a fresh empty one if absent/unreadable/corrupt.
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(Self::path(dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Best-effort persist (pretty-printed for human inspection).
    pub fn save(&self, dir: &Path) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(dir), s);
        }
    }

    /// Cumulative session count across ALL prior completed/in-flight runs.
    /// (Consumed by tests today; the natural consumer is a future `agg history`
    /// summary command — kept here so the ledger is the single source of truth.)
    #[allow(dead_code)]
    pub fn lifetime_sessions(&self) -> u32 {
        self.runs.iter().map(|r| r.sessions).sum()
    }

    /// Cumulative output tokens across all runs.
    #[allow(dead_code)]
    pub fn lifetime_tokens(&self) -> u64 {
        self.runs.iter().map(|r| r.tokens).sum()
    }
}

/// RAII handle that owns the in-flight run's slot in the ledger. Created at
/// launch (appends a record with `end_reason="crashed"` as the pessimistic
/// default), updated each session, and finalized on Drop — so even a panic or a
/// hard exit leaves an attributable record. The happy paths call `finish()` to
/// stamp the real end reason before Drop.
pub struct RunLedger {
    dir: PathBuf,
    /// index of this run's record within `project.runs`.
    idx: usize,
    proj: Project,
    finished: bool,
}

impl RunLedger {
    /// Open the ledger and append an in-flight record for this run.
    pub fn begin(dir: &Path, name: &str, pid: u32, started_at_epoch: u64) -> Self {
        let mut proj = Project::load(dir);
        if proj.created_at_epoch == 0 {
            proj.created_at_epoch = started_at_epoch;
        }
        proj.name = name.to_string();
        let run = proj.runs.len() as u32 + 1;
        proj.runs.push(RunRecord {
            run,
            started_at_epoch,
            ended_at_epoch: 0,
            pid,
            sessions: 0,
            tokens: 0,
            goals_met: 0,
            goals_total: 0,
            end_reason: "crashed".into(), // pessimistic until finish() overrides
        });
        let idx = proj.runs.len() - 1;
        proj.save(dir);
        Self { dir: dir.to_path_buf(), idx, proj, finished: false }
    }

    /// Lifetime session total INCLUDING runs before this one (this run starts at 0).
    pub fn prior_lifetime_sessions(&self) -> u32 {
        self.proj
            .runs
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.idx)
            .map(|(_, r)| r.sessions)
            .sum()
    }

    /// Update the in-flight record's running counters and persist. Cheap enough
    /// to call once per session boundary.
    pub fn update(&mut self, sessions: u32, tokens: u64, goals_met: usize, goals_total: usize) {
        if let Some(rec) = self.proj.runs.get_mut(self.idx) {
            rec.sessions = sessions;
            rec.tokens = tokens;
            rec.goals_met = goals_met;
            rec.goals_total = goals_total;
        }
        self.proj.save(&self.dir);
    }

    /// Stamp the real end reason + end time. Called on a clean exit; Drop then
    /// persists. If the process dies before this, the record keeps "crashed".
    pub fn finish(&mut self, ended_at_epoch: u64, end_reason: &str) {
        if let Some(rec) = self.proj.runs.get_mut(self.idx) {
            rec.ended_at_epoch = ended_at_epoch;
            rec.end_reason = end_reason.to_string();
        }
        self.finished = true;
        self.proj.save(&self.dir);
    }
}

impl Drop for RunLedger {
    fn drop(&mut self) {
        // Guarantee the record is persisted even on an unexpected unwind. If
        // finish() ran, this just re-saves the finalized record; if not, the
        // pessimistic "crashed" end_reason stands and we stamp an end time so the
        // record isn't left dangling with ended_at_epoch=0.
        if !self.finished {
            if let Some(rec) = self.proj.runs.get_mut(self.idx) {
                if rec.ended_at_epoch == 0 {
                    rec.ended_at_epoch = now_epoch();
                }
            }
        }
        self.proj.save(&self.dir);
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        // per-test dir under the system temp (tests share a process, so the dir
        // must be unique per test to avoid cross-test ledger contamination).
        let p = std::env::temp_dir().join(format!("agg-proj-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join(".agg")).unwrap();
        p
    }

    #[test]
    fn ledger_accumulates_across_runs() {
        let dir = tmpdir("accum");
        // run 1: 4 sessions
        {
            let mut l = RunLedger::begin(&dir, "proj", 100, 1_000);
            assert_eq!(l.prior_lifetime_sessions(), 0);
            l.update(4, 400, 1, 3);
            l.finish(1_500, "stopped");
        }
        // run 2: starts knowing 4 prior sessions
        {
            let mut l = RunLedger::begin(&dir, "proj", 200, 2_000);
            assert_eq!(l.prior_lifetime_sessions(), 4);
            l.update(3, 300, 2, 3);
            l.finish(2_500, "goals-met");
        }
        let proj = Project::load(&dir);
        assert_eq!(proj.runs.len(), 2);
        assert_eq!(proj.lifetime_sessions(), 7);
        assert_eq!(proj.lifetime_tokens(), 700);
        assert_eq!(proj.created_at_epoch, 1_000);
        assert_eq!(proj.runs[0].end_reason, "stopped");
        assert_eq!(proj.runs[1].end_reason, "goals-met");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unfinished_run_is_marked_crashed() {
        let dir = tmpdir("crashed");
        {
            let mut l = RunLedger::begin(&dir, "proj", 1, 10);
            l.update(2, 50, 0, 3);
            // drop WITHOUT finish() — simulates a crash
        }
        let proj = Project::load(&dir);
        assert_eq!(proj.runs.len(), 1);
        assert_eq!(proj.runs[0].end_reason, "crashed");
        assert!(proj.runs[0].ended_at_epoch >= 10); // stamped on drop
        let _ = std::fs::remove_dir_all(&dir);
    }
}
