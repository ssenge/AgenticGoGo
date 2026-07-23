//! `LiveState` — the shared, lock-guarded dashboard state with a throttled publisher.
//!
//! ONE of these is created by the loop and cloned into the worker; both sides mutate the same
//! [`DashboardState`] snapshot under the lock and publish through it. Keeping the single-writer
//! discipline here (bump `seq`, refresh time, atomic write) is what keeps the file consistent and
//! the live `now`/`think` fields populated mid-session.

use super::DashboardState;
use crate::util::now_epoch;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Shared, lock-guarded dashboard state with a throttled publisher. ONE of these is
/// created by the loop and cloned into the worker; both sides mutate the same snapshot
/// and publish through it. `publish` bumps `seq`, refreshes `up_secs`/`idle_secs`, and
/// writes `agg/state/state.json` atomically.
#[derive(Clone)]
pub struct LiveState {
    inner: Arc<Mutex<DashboardState>>,
    dir: PathBuf,
    loop_start: Instant,
    started_at_epoch: u64,
    /// last time a live (throttled) publish hit disk; gates the worker's repaint rate
    last_publish: Arc<Mutex<Instant>>,
}

impl LiveState {
    pub fn new(dir: &Path, loop_start: Instant, seed: DashboardState) -> Self {
        let started_at_epoch = now_epoch();
        let mut seed = seed;
        seed.started_at_epoch = started_at_epoch;
        LiveState {
            inner: Arc::new(Mutex::new(seed)),
            dir: dir.to_path_buf(),
            loop_start,
            started_at_epoch,
            // far enough in the past that the first throttled publish always fires
            last_publish: Arc::new(Mutex::new(loop_start)),
        }
    }

    /// Mutate the snapshot under lock, then publish (bump seq, refresh time, write).
    pub fn update<F: FnOnce(&mut DashboardState)>(&self, f: F) {
        {
            let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut s);
        }
        self.publish();
    }

    /// Like [`update`], but only writes to disk if at least `min_interval` has elapsed
    /// since the last (throttled) publish. The in-memory mutation ALWAYS applies; only
    /// the disk write + seq bump are throttled. Used by the high-frequency event stream
    /// so we don't write the file on every single token.
    pub fn update_throttled<F: FnOnce(&mut DashboardState)>(&self, min_interval: std::time::Duration, f: F) {
        {
            let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut s);
        }
        let mut last = self.last_publish.lock().unwrap_or_else(|e| e.into_inner());
        if last.elapsed() >= min_interval {
            *last = Instant::now();
            drop(last);
            self.publish();
        }
    }

    /// Force a publish to disk regardless of throttle (e.g. at a session boundary, so
    /// the last live event is never stuck behind the throttle window). Bumps `seq` and
    /// refreshes `up_secs`; `idle_secs` is set by callers (the worker knows it).
    pub fn publish(&self) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.seq += 1;
        s.up_secs = self.loop_start.elapsed().as_secs();
        s.started_at_epoch = self.started_at_epoch;
        let _ = s.write(&self.dir);
    }
}
