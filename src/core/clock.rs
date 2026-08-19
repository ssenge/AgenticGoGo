//! The run clock — end-to-end wall time, and how much of it was spent waiting for a human.
//!
//! `internal/HUMAN_LOOP.md` §7.4 defines three quantities, all in **seconds**:
//!
//! | term | meaning |
//! |---|---|
//! | `wall_time` | now − run start, human waiting **included**. A deadline. |
//! | `human_wait_time` | time accumulated inside blocking `hil_*` calls, across resumes. |
//! | `work_time` | `wall_time − human_wait_time`. Time the loop was actually working. |
//!
//! # Why this is a file on disk and not an `Instant`
//!
//! `wall_time` is *defined* as end-to-end, and a resumed run is a new process. An `Instant` restarts
//! with it, so the old `wall_hours` handed every resumed run a fresh full allowance — tolerable while
//! resume was only the crash path, and unacceptable once a human answering a question is a normal way
//! for a run to pause. Epoch-on-disk is what makes the number mean what its name says.
//!
//! # Why `work_time` has to exist as its own term
//!
//! Ceilings keep firing while a `hil_*` call is blocked — verified on the wire, not assumed. With an
//! e2e clock alone, one question asked at 23:00 and answered at 08:00 burns nine hours of a ceiling
//! that was measuring the agent's effort, and a healthy run dies because a person slept. The
//! condition grammar has comparisons but no arithmetic, so `wall_time - human_wait_time >= 28800`
//! cannot be written and `work_time` must be a term of its own.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The persisted run clock. Agg-owned (`agg/private/clock.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Clock {
    /// epoch seconds at which this run — including everything it has resumed from — began.
    pub started_at_epoch: u64,
    /// seconds spent inside blocking human calls, accumulated across resumes.
    #[serde(default)]
    pub human_wait_secs: f64,
}

impl Clock {
    /// Load the clock for a run that is starting.
    ///
    /// `fresh` starts a new clock (a normal launch); otherwise an existing file is reused, which is
    /// what carries `started_at_epoch` and the accumulated human wait across a resume. A missing or
    /// unparseable file always starts fresh — a corrupt clock must not stop a run.
    pub fn open(dir: &Path, now_epoch: u64, fresh: bool) -> Clock {
        if !fresh {
            if let Some(c) = std::fs::read_to_string(crate::paths::clock_json(dir))
                .ok()
                .and_then(|s| serde_json::from_str::<Clock>(&s).ok())
                .filter(|c| c.started_at_epoch > 0)
            {
                return c;
            }
        }
        let c = Clock { started_at_epoch: now_epoch, human_wait_secs: 0.0 };
        c.save(dir);
        c
    }

    /// Persist. Best-effort: a clock that cannot be written costs accuracy on the next resume, and
    /// killing a run over it would be a worse trade.
    pub fn save(&self, dir: &Path) {
        let path = crate::paths::clock_json(dir);
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = crate::util::write_atomic(&path, &s);
        }
    }

    /// Add human wait and persist, so the accumulation survives a crash mid-wait.
    pub fn add_human_wait(&mut self, dir: &Path, secs: f64) {
        if secs <= 0.0 {
            return;
        }
        self.human_wait_secs += secs;
        self.save(dir);
    }

    /// End-to-end seconds since the run began. Clamped at 0: a clock moved backwards (NTP, a
    /// hand-edited file) must not produce a negative ceiling that reads as "unlimited".
    pub fn wall_secs(&self, now_epoch: u64) -> f64 {
        now_epoch.saturating_sub(self.started_at_epoch) as f64
    }

    /// Seconds the loop was actually working. Clamped at 0 for the same reason.
    pub fn work_secs(&self, now_epoch: u64) -> f64 {
        (self.wall_secs(now_epoch) - self.human_wait_secs).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the file: a "resumed" open keeps the original start and the wait already
    /// banked, so `wall_time` stays end-to-end instead of restarting with the process.
    #[test]
    fn a_resumed_clock_keeps_its_start_and_its_banked_wait() {
        let d = tempfile::tempdir().unwrap();
        let mut c = Clock::open(d.path(), 1_000, true);
        c.add_human_wait(d.path(), 120.0);

        let resumed = Clock::open(d.path(), 9_999, false);
        assert_eq!(resumed.started_at_epoch, 1_000, "a resume must not restart the clock");
        assert_eq!(resumed.human_wait_secs, 120.0, "banked human wait survives the process");

        // e2e counts the gap; work_time does not count the waiting.
        assert_eq!(resumed.wall_secs(1_600), 600.0);
        assert_eq!(resumed.work_secs(1_600), 480.0);
    }

    /// `fresh: true` is a normal launch and must NOT inherit a previous run's clock.
    #[test]
    fn a_fresh_launch_ignores_an_existing_clock() {
        let d = tempfile::tempdir().unwrap();
        let mut c = Clock::open(d.path(), 1_000, true);
        c.add_human_wait(d.path(), 500.0);
        let next = Clock::open(d.path(), 5_000, true);
        assert_eq!(next.started_at_epoch, 5_000);
        assert_eq!(next.human_wait_secs, 0.0, "a new run starts owing nothing");
    }

    /// A clock that ran backwards must read as 0, never as a negative that compares below every
    /// ceiling and silently means "unlimited".
    #[test]
    fn a_backwards_clock_clamps_to_zero() {
        let c = Clock { started_at_epoch: 9_000, human_wait_secs: 100.0 };
        assert_eq!(c.wall_secs(1_000), 0.0);
        assert_eq!(c.work_secs(1_000), 0.0);
    }
}
