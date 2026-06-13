//! Dashboard state — the serializable snapshot the loop writes and the TUI reads.
//!
//! Two-stream discipline (the hard lesson from a prior harness): the line-oriented log
//! on stdout stays the source of truth (greppable, tailable). The TUI is a *view*
//! rendered from this compact state file, never the only output. The loop writes
//! `.agg/state.json` atomically after each meaningful change; `agg dashboard`
//! polls it and repaints in place.
//!
//! Single-writer-under-lock: both the loop (boundary updates) and the worker's reader
//! thread (live activity, mid-session) mutate ONE `Arc<Mutex<DashboardState>>` and
//! publish through [`LiveState`]. That removes the dual-writer `seq`/torn-file race the
//! old design had (the loop published only at session boundaries, so `now`/`think` were
//! empty during a session).

use crate::engine::Engine;
use crate::model::{GoalType, Lifecycle};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Recent-events ring cap — how many activity lines the dashboard tail carries.
pub const RECENT_CAP: usize = 50;

/// A single goal's view for the dashboard.
///
/// `#[serde(default)]` so a state.json written by an older `agg` (missing `weight`,
/// etc.) still deserializes — the dashboard and loop binaries can differ across an
/// in-place upgrade mid-run, and a parse failure would blank the whole dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GoalView {
    pub id: String,
    pub goal_type: String,   // "binary"/"percentage"/"cardinal"
    pub state: String,       // lifecycle: "pending"/"in_progress"/"met"/"regressed"
    pub invariant: bool,
    pub value: f64,
    pub max: f64,
    pub target: f64,
    pub weight: f64,
    pub delta: f64,          // change in value since last cycle (for ▲+N)
    pub rationale: String,
    pub judge_kind: String,  // "script" | "llm:<model>"
    /// true when a `recheck: once_met` goal is latched — judge no longer re-runs.
    #[serde(default)]
    pub latched: bool,
}

/// One formatted activity line for the real-time tail. `kind` is the leading glyph
/// category so the dashboard can color it; `text` is the line WITHOUT the glyph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ActivityEvent {
    pub ts: String,   // HH:MM:SS
    pub kind: String, // "tool" | "think" | "result" | "tool_result" | "init"
    pub text: String,
}

/// The full dashboard snapshot.
///
/// `#[serde(default)]` for forward/backward compatibility: a snapshot written by a
/// different `agg` version (older = missing the live-activity fields, newer = extra
/// fields) still deserializes cleanly rather than blanking the dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardState {
    pub project: String,
    pub model: String,
    pub stop_when: String,
    pub halt_when: String,
    /// epoch secs the loop started (for an absolute "started at" in the Info block)
    pub started_at_epoch: u64,
    pub up_secs: u64,
    /// session number WITHIN the current `agg run` invocation (resets to 0 each run).
    pub session: u32,
    /// cumulative session count across ALL `agg run` invocations for this project
    /// (persisted in `.agg/sessions.count`). Survives restarts so the dashboard can
    /// show "how many sessions has this project ever run", not just this invocation.
    pub lifetime_session: u32,
    pub phase: String,       // "running" | "judging" | "backoff" | "done" | ...
    pub idle_secs: u64,
    pub tokens_spent: u64,
    pub budget_total: Option<u64>,
    pub goals_met: usize,
    pub goals_total: usize,
    pub goals: Vec<GoalView>,
    pub now: String,         // current activity line (last 🔧/💬)
    pub think: String,       // last 💬 thought
    /// rolling tail of recent formatted events (capped at [`RECENT_CAP`]) — drives the
    /// real-time Activity panel. Oldest first; the dashboard renders the last N.
    pub recent: Vec<ActivityEvent>,
    pub summary_cumulative: String,
    pub summary_windowed: String,
    /// monotonically increasing; lets the dashboard detect updates
    pub seq: u64,
    /// terminal flag — dashboard shows the final banner and can exit
    pub finished: bool,
    pub finish_reason: String,
}

impl DashboardState {
    /// Path to the state file under a project dir.
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(".agg").join("state.json")
    }

    /// Snapshot the current goal set from the engine into goal views.
    pub fn goals_from_engine(eng: &Engine, prev: &[GoalView]) -> Vec<GoalView> {
        eng.goals
            .iter()
            .map(|g| {
                let value = g.last_verdict.as_ref().map(|v| v.value).unwrap_or(0.0);
                let prev_value = prev.iter().find(|p| p.id == g.id).map(|p| p.value).unwrap_or(value);
                let judge_kind = match &g.judge {
                    crate::model::JudgeSpec::Script { .. } => "script".to_string(),
                    crate::model::JudgeSpec::Llm { model, .. } => format!("llm:{model}"),
                };
                GoalView {
                    id: g.id.clone(),
                    goal_type: type_str(g.goal_type),
                    state: state_str(g.state),
                    invariant: g.invariant,
                    value,
                    max: g.last_verdict.as_ref().map(|v| v.max).unwrap_or(1.0),
                    target: g.target,
                    weight: g.weight,
                    delta: value - prev_value,
                    rationale: g.last_verdict.as_ref().map(|v| v.rationale.clone()).unwrap_or_default(),
                    judge_kind,
                    latched: g.latched,
                }
            })
            .collect()
    }

    /// Push one activity event onto the rolling tail, capping at [`RECENT_CAP`]
    /// (drops the oldest). Also keeps `now`/`think` in sync as a convenience.
    pub fn push_event(&mut self, ev: ActivityEvent) {
        match ev.kind.as_str() {
            "think" => {
                self.think = ev.text.clone();
                self.now = ev.text.clone();
            }
            "tool" => self.now = ev.text.clone(),
            _ => {}
        }
        self.recent.push(ev);
        if self.recent.len() > RECENT_CAP {
            let overflow = self.recent.len() - RECENT_CAP;
            self.recent.drain(0..overflow);
        }
    }

    /// Write atomically (write tmp, rename) so the dashboard never reads a torn file.
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        let dest = Self::path(dir);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = dest.with_extension("json.tmp");
        let json = serde_json::to_string(self).unwrap_or_else(|_| "{}".into());
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &dest)
    }

    /// Read the latest state, or None if missing/unparseable.
    pub fn read(dir: &Path) -> Option<DashboardState> {
        let text = std::fs::read_to_string(Self::path(dir)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// Shared, lock-guarded dashboard state with a throttled publisher. ONE of these is
/// created by the loop and cloned into the worker; both sides mutate the same snapshot
/// and publish through it. `publish` bumps `seq`, refreshes `up_secs`/`idle_secs`, and
/// writes `.agg/state.json` atomically.
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

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn type_str(t: GoalType) -> String {
    match t {
        GoalType::Binary => "binary",
        GoalType::Percentage => "percentage",
        GoalType::Cardinal => "cardinal",
    }
    .to_string()
}

fn state_str(s: Lifecycle) -> String {
    match s {
        Lifecycle::Pending => "pending",
        Lifecycle::InProgress => "in_progress",
        Lifecycle::Met => "met",
        Lifecycle::Regressed => "regressed",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state.json written by an OLDER `agg` (no model/halt_when/started_at/recent,
    /// goals without `weight`) must still deserialize — the new fields fall back to
    /// their defaults via `#[serde(default)]`. Guards the in-place-upgrade-mid-run case.
    #[test]
    fn old_schema_deserializes_with_defaults() {
        let old = r#"{
            "project":"telos","stop_when":"mip28_optimal","up_secs":4705,"session":2,
            "phase":"judging","idle_secs":0,"tokens_spent":588117,"budget_total":null,
            "goals_met":2,"goals_total":3,
            "goals":[{"id":"g","goal_type":"cardinal","state":"in_progress","invariant":false,
                      "value":18.0,"max":28.0,"target":28.0,"delta":0.0,
                      "rationale":"18/28","judge_kind":"script"}],
            "now":"x","think":"y","summary_cumulative":"s","summary_windowed":"w",
            "seq":12,"finished":false,"finish_reason":""
        }"#;
        let s: DashboardState = serde_json::from_str(old).expect("old schema must parse");
        assert_eq!(s.project, "telos");
        assert_eq!(s.session, 2);
        // new fields defaulted, not errored:
        assert_eq!(s.model, "");
        assert_eq!(s.halt_when, "");
        assert_eq!(s.started_at_epoch, 0);
        assert!(s.recent.is_empty());
        // a goal missing `weight` defaults to 0.0 (rendered as "w0").
        assert_eq!(s.goals[0].weight, 0.0);
        assert_eq!(s.goals[0].value, 18.0);
    }

    #[test]
    fn push_event_caps_the_ring_and_tracks_now_think() {
        let mut s = DashboardState::default();
        for i in 0..(RECENT_CAP + 10) {
            s.push_event(ActivityEvent {
                ts: "00:00:00".into(),
                kind: "tool".into(),
                text: format!("cmd {i}"),
            });
        }
        assert_eq!(s.recent.len(), RECENT_CAP); // capped
        assert_eq!(s.recent.last().unwrap().text, format!("cmd {}", RECENT_CAP + 9)); // newest kept
        assert_eq!(s.recent.first().unwrap().text, "cmd 10"); // oldest dropped
        assert_eq!(s.now, format!("cmd {}", RECENT_CAP + 9)); // tool updates `now`

        s.push_event(ActivityEvent { ts: "00:00:01".into(), kind: "think".into(), text: "pondering".into() });
        assert_eq!(s.think, "pondering"); // think updates `think` (and `now`)
        assert_eq!(s.now, "pondering");
    }

    #[test]
    fn live_state_publishes_to_disk() {
        let dir = std::env::temp_dir().join(format!("agg_live_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let live = LiveState::new(&dir, Instant::now(), DashboardState { project: "p".into(), ..Default::default() });
        live.update(|s| s.push_event(ActivityEvent { ts: "t".into(), kind: "tool".into(), text: "go".into() }));
        let read = DashboardState::read(&dir).expect("state.json written");
        assert_eq!(read.project, "p");
        assert_eq!(read.recent.len(), 1);
        assert_eq!(read.now, "go");
        assert!(read.seq >= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
