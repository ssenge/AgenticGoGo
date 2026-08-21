//! Dashboard state — the serializable snapshot the loop writes and the TUI reads.
//!
//! Two-stream discipline: the line-oriented log on stdout stays the source of truth
//! (greppable, tailable). The TUI is a *view* rendered from this compact state file, never
//! the only output. The loop writes `agg/private/state.json` atomically after each meaningful change;
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

mod live;
mod phase;
pub use live::*;
pub use phase::*;

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
    /// A Rust DRIVER's position in its own control flow — `cycle 3/20 › attempt 2/3` — assembled from
    /// the live [`crate::driver::PosFrame`] stack (`Agg::label_path`). Empty on the YAML path and for
    /// any driver that declares no `pos()`: agg CANNOT see a hand-written `for`, so a loop with no
    /// breadcrumb shows no counter rather than a fabricated one.
    ///
    /// It is the only progress signal that exists for driver flow — `session N` counts sessions, not
    /// cycles — and Phase 1 keys the call ledger's identity check on this same string (BUILD.md
    /// §3.4), which is why it has to be a published, observable value and not a debug helper.
    pub pos: String,
    pub stop_when: String,
    pub halt_when: String,
    /// the isolation BASE branch this run merges into — the branch `gate()` stages onto and the one
    /// the run-end warning says is unchanged.
    ///
    /// It is RECORDED here for the next run, not for a reader: `base_branch` defaults to empty, so a
    /// run that starts with HEAD stranded on a crashed run's session branch would otherwise resolve
    /// its base FROM that dead branch — every gate would merge into it, report `Kept`, and never
    /// touch `main` while nothing warned (BUILD.md §3.9 rule 1). `GitSetup` reads this field back to
    /// recover the real base.
    pub iso_base: String,
    /// epoch secs the loop started (for an absolute "started at" in the Info block)
    pub started_at_epoch: u64,
    pub up_secs: u64,
    /// session number WITHIN the current `agg run` invocation (resets to 0 each run).
    pub session: u32,
    /// cumulative session count across ALL `agg run` invocations for this project
    /// (persisted in the run ledger `agg/private/project.json`). Survives restarts so the dashboard can
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
    /// FLAGGED FOR HELP: the session a `notify_if` last fired on, and the `{{reason}}` it
    /// delivered. `None` = the loop has never asked for a human this run.
    ///
    /// A FIELD, deliberately not a `Phase` variant: `phase` says WHERE the loop currently is, and
    /// notify fires *inside* Gate — a `Phase::Notify` would overwrite `Gate` and tell every reader
    /// the loop is somewhere it isn't. This is a flag that persists after the phase moves on, which
    /// is exactly what an operator glancing at the dashboard needs: not "it pinged for one instant"
    /// but "it is asking for you, and has been since session N".
    pub notify_session: Option<u32>,
    pub notify_reason: String,
    /// OPEN HUMAN ASKS — questions a human still owes an answer to, oldest first.
    ///
    /// With no timeout on a `hil_*` call, a forgotten ask hangs the run indefinitely. That is the
    /// accepted cost of the design, and this field is most of the mitigation: `agg status` and the
    /// web reader must show the question and how long it has been waiting, prominently, so "blocked
    /// on a human" is never a silent state.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asks: Vec<AskView>,
    /// monotonically increasing; lets the dashboard detect updates
    pub seq: u64,
    /// terminal flag — dashboard shows the final banner and can exit
    pub finished: bool,
    pub finish_reason: String,
}

/// One open ask, flattened for the readers. Mirrors [`crate::core::asks::Ask`] minus what a reader
/// has no use for, and carries `age_secs` pre-computed so every reader agrees on the number.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AskView {
    pub id: String,
    /// `bool` | `choose` | `input` — what a UI should render.
    pub case: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// `driver` | `worker` — a worker asking every session is visible here.
    pub origin: String,
    pub session: u32,
    pub age_secs: u64,
}

impl AskView {
    pub fn of(a: &crate::core::asks::Ask, now_epoch: u64) -> Self {
        use crate::core::asks::{AskCase, Origin};
        AskView {
            id: a.id.clone(),
            case: match a.case {
                AskCase::Bool => "bool",
                AskCase::Choose => "choose",
                AskCase::Input => "input",
            }
            .into(),
            question: crate::core::asks::one_line(&a.question, 300),
            options: a.options.clone(),
            origin: match a.origin {
                Origin::Driver => "driver",
                Origin::Worker => "worker",
            }
            .into(),
            session: a.session,
            age_secs: a.age_secs(now_epoch),
        }
    }
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
mod tests;
