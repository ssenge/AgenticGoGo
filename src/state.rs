//! Dashboard state — the serializable snapshot the loop writes and the TUI reads.
//!
//! Two-stream discipline: the line-oriented log on stdout stays the source of truth
//! (greppable, tailable). The TUI is a *view* rendered from this compact state file, never
//! the only output. The loop writes `agg/state/state.json` atomically after each meaningful change;
//! `agg dashboard` polls it and repaints in place.
//!
//! Single-writer-under-lock: both the loop (boundary updates) and the worker's reader thread
//! (live activity, mid-session) mutate ONE `Arc<Mutex<DashboardState>>` and publish through
//! [`LiveState`]. A single writer under the lock is what keeps the `seq`/file write consistent
//! and the live `now`/`think` fields populated mid-session.

use crate::core::engine::Engine;
use crate::core::model::Lifecycle;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

/// One judge's view for the §7.4 per-judge scoreboard — the successor to [`GoalView`].
///
/// Three things the old `GoalView` could not say, that §7.4 requires:
///   * `value`/`max` are `Option`: a *binary* judge emits NO number and a *broken* judge emits
///     nothing usable, so both are `None` — rendered `met`/`unmet`, NEVER a lying `0` (a real
///     measured `0.0` is a distinct, different thing).
///   * `met` is explicit, so a reader never has to infer it from the `state` string.
///   * `error` carries a broken judge's reason (`Some` ⇒ "I could not grade this", not "not met").
///
/// `in_dod` lets the reader mark a run-set-only control judge (e.g. `stalled`) apart from the
/// DoD-set the aggregates range over (§5.3) — so "why we stalled" is surfaceable without polluting
/// the `judges met/total` count.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JudgeView {
    pub name: String,
    pub kind: String, // "script" | "llm"
    pub in_dod: bool,
    pub invariant: bool,
    pub state: String, // "pending"/"in_progress"/"met"/"regressed"
    pub met: bool,
    /// numeric measure, or `None` for a binary/errored judge — NOT `0` (§7.4).
    pub value: Option<f64>,
    pub max: Option<f64>,
    pub target: f64,
    pub delta: f64,
    pub rationale: String,
    /// set when the judge itself failed to run (vs. a clean "not met").
    pub error: Option<String>,
}

impl JudgeView {
    /// Bridge a legacy [`GoalView`] into a judge view, so a state.json written by a pre-§7.4 `agg`
    /// still renders on the new scoreboard. Recovers the lost Option-ness from the goal type: a
    /// `binary` goal carried no real number, so `value`/`max` come back as `None`.
    fn from_goal(g: &GoalView) -> Self {
        let numeric = g.goal_type != "binary";
        JudgeView {
            name: g.id.clone(),
            kind: g.judge_kind.clone(),
            in_dod: true, // the legacy `goals` set was DoD-only (state.rs filtered to `in_dod`).
            invariant: g.invariant,
            state: g.state.clone(),
            met: g.state == "met",
            value: numeric.then_some(g.value),
            max: numeric.then_some(g.max),
            target: g.target,
            delta: g.delta,
            rationale: g.rationale.clone(),
            error: None,
        }
    }
}

/// One agent's token + cost tally for the §7.4 per-agent breakdown. `cost` is `Option` because an
/// agent that cannot report a price (a subscription CLI) must show "—", never a lying `$0.00`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentUsage {
    pub tokens: u64,
    pub cost: Option<f64>,
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
    /// the CURRENT step's name (§7.4) — empty before the first step / off-cycle. The step's AGENT
    /// is already in the session banner (the source-of-truth log, per the two-stream discipline
    /// above); a per-agent token/cost BREAKDOWN is deferred to the UI workflow — the aggregate
    /// `tokens_spent`/`cost_spent` below already sum worker + judge across agents (§5.6), so the
    /// spend guards stay correct without it.
    pub step: String,
    /// the current step's AGENT and its resolved MODEL (§7.4). A mixed claude/codex run is
    /// uninterpretable without knowing who ran THIS step. Empty before the first step / off-cycle.
    pub step_agent: String,
    pub step_model: String,
    pub stop_when: String,
    pub halt_when: String,
    /// epoch secs the loop started (for an absolute "started at" in the Info block)
    pub started_at_epoch: u64,
    pub up_secs: u64,
    /// session number WITHIN the current `agg run` invocation (resets to 0 each run).
    pub session: u32,
    /// cumulative session count across ALL `agg run` invocations for this project
    /// (persisted in `agg/state/sessions.count`). Survives restarts so the dashboard can
    /// show "how many sessions has this project ever run", not just this invocation.
    pub lifetime_session: u32,
    /// the outer loop's current stage.
    pub phase: Phase,
    pub idle_secs: u64,
    pub tokens_spent: u64,
    pub budget_total: Option<u64>,
    /// cumulative dollars spent this run (`total_cost_usd` summed); 0.0 when unknown.
    pub cost_spent: f64,
    /// the dollar ceiling (`cost.total`), if configured.
    pub cost_limit: Option<f64>,
    pub goals_met: usize,
    pub goals_total: usize,
    pub goals: Vec<GoalView>,
    /// the §7.4 per-judge scoreboard — the successor to `goals` (Option value/max, explicit `met`,
    /// broken-judge `error`, and run-set judges like `stalled`). `goals` stays populated alongside
    /// until every reader (the web UI still reads `goals`) has migrated; §7.1 forbids a bridge, so
    /// this is a superset window, not a translation layer.
    pub judges: Vec<JudgeView>,
    /// per-agent token + cost breakdown (§7.4); sums to `tokens_spent`/`cost_spent`. Empty on a
    /// single-agent run, or a state.json written before per-agent accounting existed.
    pub per_agent: BTreeMap<String, AgentUsage>,
    pub now: String,         // current activity line (last 🔧/💬)
    pub think: String,       // last 💬 thought
    /// rolling tail of recent formatted events (capped at [`RECENT_CAP`]) — drives the
    /// real-time Activity panel. Oldest first; the dashboard renders the last N.
    pub recent: Vec<ActivityEvent>,
    pub summary_cumulative: String,
    pub summary_windowed: String,
    /// size (bytes) of the durable `LOG.md` after the last fold — surfaces "how much
    /// institutional memory has accumulated" (and how close to the cap) on the dashboard/status.
    /// 0 when memory is empty or a write failed. Named `_bytes` (not `_chars`) because it is a
    /// byte length, displayed as B/KB.
    pub memory_bytes: usize,
    /// monotonically increasing; lets the dashboard detect updates
    pub seq: u64,
    /// terminal flag — dashboard shows the final banner and can exit
    pub finished: bool,
    pub finish_reason: String,
}

impl DashboardState {
    /// Path to the state file under a project dir.
    pub fn path(dir: &Path) -> PathBuf {
        crate::paths::state_json(dir)
    }

    /// Snapshot the current judge set from the engine into goal views. Only the DoD-set judges are
    /// shown as goals; a run-set-only judge (e.g. `stalled`) is machinery, not a goal.
    pub fn goals_from_engine(eng: &Engine, prev: &[GoalView]) -> Vec<GoalView> {
        eng.judges
            .iter()
            .filter(|g| g.in_dod)
            .map(|g| {
                let value = g.last_verdict.as_ref().and_then(|v| v.value).unwrap_or(0.0);
                let prev_value = prev.iter().find(|p| p.id == g.name).map(|p| p.value).unwrap_or(value);
                GoalView {
                    id: g.name.clone(),
                    goal_type: g.type_str().to_string(),
                    state: state_str(g.state),
                    invariant: g.invariant,
                    value,
                    max: g.last_verdict.as_ref().and_then(|v| v.max).unwrap_or(1.0),
                    target: g.last_verdict.as_ref().map(|v| v.target).unwrap_or(1.0),
                    weight: 1.0,
                    delta: value - prev_value,
                    rationale: g.last_verdict.as_ref().map(|v| v.rationale.clone()).unwrap_or_default(),
                    judge_kind: g.kind.tag().to_string(),
                    latched: false,
                }
            })
            .collect()
    }

    /// Snapshot the FULL judge set (§7.4) into judge views — both the DoD-set AND the run-set, so a
    /// control judge like `stalled` (`in_dod: false`) is visible and the reader can surface "why we
    /// stalled". Unlike [`Self::goals_from_engine`], this preserves the Option-ness of `value`/`max`:
    /// a binary or broken judge stays numberless on the wire, so the reader renders met/unmet rather
    /// than a fabricated `0`.
    pub fn judges_from_engine(eng: &Engine, prev: &[JudgeView]) -> Vec<JudgeView> {
        eng.judges
            .iter()
            .map(|g| {
                let v = g.last_verdict.as_ref();
                let value = v.and_then(|v| v.value);
                // delta only where both this and the prior step carried a real number.
                let prev_value = prev.iter().find(|p| p.name == g.name).and_then(|p| p.value);
                let delta = match (value, prev_value) {
                    (Some(now), Some(was)) => now - was,
                    _ => 0.0,
                };
                JudgeView {
                    name: g.name.clone(),
                    kind: g.kind.tag().to_string(),
                    in_dod: g.in_dod,
                    invariant: g.invariant,
                    state: state_str(g.state),
                    met: g.met(),
                    value,
                    max: v.and_then(|v| v.max),
                    target: v.map(|v| v.target).unwrap_or(1.0),
                    delta,
                    rationale: v.map(|v| v.rationale.clone()).unwrap_or_default(),
                    error: v.and_then(|v| v.error.clone()),
                }
            })
            .collect()
    }

    /// The per-judge scoreboard the readers render (§7.4). Prefers the native `judges`; falls back
    /// to mapping the legacy `goals` so a state.json written by a pre-§7.4 `agg` still renders
    /// (the cross-version contract the whole struct is `#[serde(default)]` for).
    pub fn judge_views(&self) -> Vec<JudgeView> {
        if !self.judges.is_empty() {
            return self.judges.clone();
        }
        self.goals.iter().map(JudgeView::from_goal).collect()
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

use crate::util::now_epoch;

fn state_str(s: Lifecycle) -> String {
    match s {
        Lifecycle::Pending => "pending",
        Lifecycle::InProgress => "in_progress",
        Lifecycle::Met => "met",
        Lifecycle::Regressed => "regressed",
    }
    .to_string()
}

/// The outer loop's current stage. The four deterministic stages (INJECT → RUN → VERIFY → GATE)
/// plus the three off-cycle ones.
///
/// Was a bare `String` assigned from literals at ~10 sites in loop_.rs and re-matched by literal
/// in the dashboard and status renderers — a typo at either end was a silent mis-render, and
/// adding a stage meant remembering to touch two `match`es with `_` arms that would happily
/// swallow it.
///
/// # state.json compatibility (REQUIRED, both directions)
/// This serializes to and from exactly the lowercase strings it always did, because `state.json`
/// is a cross-version contract: `agg dashboard` / `agg status` attach to a loop that may be
/// running a DIFFERENT `agg` build than they are.
///
/// That is also why [`Phase::Other`] exists rather than a hard parse error. An older agg wrote
/// `"phase":"judging"` — a stage this build has no variant for. Rejecting it would crash the
/// dashboard against a running loop; mapping it to a catch-all `Unknown` would lie about what the
/// loop is doing. `Other` keeps the text verbatim, so it round-trips and still renders.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Starting,
    Inject,
    Run,
    Verify,
    Gate,
    Backoff,
    /// a `skip_judges` step whose work is staged onto the span (§7.4).
    Staging,
    Done,
    /// A stage name this build doesn't know — from a state.json written by another agg version.
    /// Held verbatim so it survives a read/write round-trip instead of being flattened.
    Other(String),
}

impl Phase {
    /// The wire form — the exact lowercase string that has always been in state.json.
    pub fn as_str(&self) -> &str {
        match self {
            Phase::Starting => "starting",
            Phase::Inject => "inject",
            Phase::Run => "run",
            Phase::Verify => "verify",
            Phase::Gate => "gate",
            Phase::Backoff => "backoff",
            Phase::Staging => "staging",
            Phase::Done => "done",
            Phase::Other(s) => s,
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for Phase {
    fn from(s: &str) -> Self {
        match s {
            "starting" => Phase::Starting,
            "inject" => Phase::Inject,
            "run" => Phase::Run,
            "verify" => Phase::Verify,
            "gate" => Phase::Gate,
            "backoff" => Phase::Backoff,
            "staging" => Phase::Staging,
            "done" => Phase::Done,
            other => Phase::Other(other.to_string()),
        }
    }
}

// Hand-written rather than `#[derive(Serialize, Deserialize)]`: a derived fieldless enum would
// reject any unknown tag, and serde's `#[serde(other)]` escape hatch is only available on
// internally/adjacently-tagged enums — neither applies to a plain JSON string field. So we go
// through `String` and let `From<&str>` absorb the unknown case.
impl serde::Serialize for Phase {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Phase {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Phase::from(String::deserialize(d)?.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire form is a cross-version contract — it must be byte-for-byte what it always was.
    #[test]
    fn phase_round_trips_through_its_legacy_wire_strings() {
        for (p, wire) in [
            (Phase::Starting, "starting"),
            (Phase::Inject, "inject"),
            (Phase::Run, "run"),
            (Phase::Verify, "verify"),
            (Phase::Gate, "gate"),
            (Phase::Backoff, "backoff"),
            (Phase::Done, "done"),
        ] {
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{wire}\""));
            assert_eq!(serde_json::from_str::<Phase>(&format!("\"{wire}\"")).unwrap(), p);
            assert_eq!(p.to_string(), wire);
        }
    }

    /// A stage written by a DIFFERENT agg build (an old one wrote "judging") must neither crash
    /// the reader nor lose its name — `agg dashboard` attaches to loops it didn't launch.
    #[test]
    fn an_unknown_phase_survives_verbatim() {
        let p: Phase = serde_json::from_str("\"judging\"").expect("must not reject a foreign stage");
        assert_eq!(p, Phase::Other("judging".into()));
        assert_eq!(p.to_string(), "judging", "it must still render its real name");
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"judging\"", "and round-trip unchanged");
    }

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
