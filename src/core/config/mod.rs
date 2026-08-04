//! Configuration loading: `agg.yaml` (harness + steps + sequence). One file now — `goals.yaml`
//! is DELETED (§7.1): a judge IS a goal, resolved by name from disk.
//!
//! `#[serde(deny_unknown_fields)]` is on EVERY struct here (§4.1): without it a stale top-level
//! `budget:` after the config move is silently ignored — an autonomous loop whose spend ceiling is
//! a decorative key. That guard is also what makes "any other key in a step body (esp `judge_*`) is
//! a HARD ERROR" true.
//!
//! This module is the SERDE SHAPE: every `agg.yaml` struct, its `Default`, and the `default_*`
//! helper fns (which must live beside the structs that name them in `#[serde(default = "…")]`),
//! plus [`ResolvedStep`]. The `impl AggConfig` behaviour (backends, step resolution, load + env
//! overrides) lives in the `methods` submodule.

use crate::backend::{for_name, AgentBackend};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

mod methods;

/// Harness configuration (`agg.yaml`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggConfig {
    pub project: String,

    /// Inherited by EVERY step; a step may override any of these (§4).
    #[serde(default)]
    pub defaults: Defaults,

    /// THE RULER — a run-level, immutable block. Naming any of these keys in a step body is a HARD
    /// ERROR (a grader that moves makes verdicts incomparable across cycles). This is what
    /// [`Self::ruler_backend`] reads.
    #[serde(default)]
    pub judge: JudgeCfg,

    /// The step palette. NAME → a body of overrides. The name is a user string literal; any key in
    /// the body other than the [`StepBody`] fields (esp `judge_*`) is a HARD ERROR (deny_unknown).
    #[serde(default)]
    pub steps: BTreeMap<String, StepBody>,

    /// The repeating list of statements + the run-level ceilings/DoD.
    pub sequence: Sequence,

    // ---- top-level survivors (unchanged, §4.1) ----
    #[serde(default = "default_heartbeat")]
    pub heartbeat_secs: u64,
    #[serde(default)]
    pub watchdog: Watchdog,
    #[serde(default = "default_backoff")]
    pub ratelimit_backoff_secs: u64,
    #[serde(default)]
    pub memory: Memory,
    #[serde(default)]
    pub hooks: Hooks,
    #[serde(default)]
    pub prompt_includes: Vec<String>,
    #[serde(default)]
    pub summary: Summary,
    #[serde(default)]
    pub session_isolation: SessionIsolation,
}

/// Values inherited by every step (§4). A step body overrides any of them.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// the WORKER default agent.
    #[serde(default = "default_agent")]
    pub agent: String,
    /// worker model; `None` = the step's backend default, resolved at USE time.
    #[serde(default)]
    pub model: Option<String>,
    /// thinking effort; `None` = the backend default; `Some("")` = pass none.
    #[serde(default)]
    pub effort: Option<String>,
    /// the sandbox constraint — inheritable, so an operator sets it once (§4.1).
    #[serde(default)]
    pub worker_args: Vec<String>,
    /// the forward state file (`state/STATE.md`), resolved against `agg/`; the AGENT writes it
    /// best-effort (§5.6). Under the gitignored `agg/state/`, so a worker's forward advice survives
    /// a session rollback (the code attempt is thrown away, the advice about it is not).
    #[serde(default = "default_state")]
    pub state: String,
    /// the generic framing for this step's ROLE (§4) — prepended as its own section above the
    /// step's own `prompt:`. Config-driven, so a role like `reconsider`/`reviewer`/`tester` needs
    /// no Rust (this replaced the hardcoded `enum Role` red-team arm). `None` = no role section.
    #[serde(default)]
    pub role_prompt: Option<String>,
    /// blast-radius isolation (`none` | `sandbox` | `container`) — the jail around the worker
    /// (DIFFERENT from `session_isolation`, which protects the git history). Inheritable; a step
    /// may override. `None` here = fall through to the [`crate::isolation::Isolation`] default (none).
    #[serde(default)]
    pub isolation: Option<crate::isolation::Isolation>,
    /// the base image an `isolation: container` step runs in. Inheritable; ignored by every other
    /// tier. `None` = [`crate::isolation::DEFAULT_IMAGE`].
    #[serde(default)]
    pub image: Option<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            agent: default_agent(),
            model: None,
            effort: None,
            worker_args: vec![],
            state: default_state(),
            role_prompt: None,
            isolation: None,
            image: None,
        }
    }
}

/// THE RULER (§4): the backend that runs LLM judges and the summarizer. Run-level and immutable.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeCfg {
    #[serde(default = "default_agent")]
    pub agent: String,
    /// `None` = the ruler's own cheap-model default, resolved at USE time.
    #[serde(default)]
    pub model: Option<String>,
    /// EVERY judge's timeout, script and LLM alike (seconds).
    #[serde(default = "default_judge_timeout")]
    pub timeout: u64,
}

impl Default for JudgeCfg {
    fn default() -> Self {
        JudgeCfg { agent: default_agent(), model: None, timeout: default_judge_timeout() }
    }
}

/// One step's body — a bag of OVERRIDES over [`Defaults`], plus `prompt`/`skip_judges` and the
/// per-step isolation lists. The COMPLETE legal key list is:
///
/// ```text
/// agent · model · effort · worker_args · state · role_prompt · prompt · skip_judges
/// isolation · image · readonly · writable
/// ```
///
/// Any other key is a HARD ERROR (deny_unknown), which is what makes naming a `judge_*` key in a
/// step fail loudly instead of being silently ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepBody {
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub worker_args: Option<Vec<String>>,
    #[serde(default)]
    pub state: Option<String>,
    /// generic ROLE framing for this step (§4) — a config-driven section above `prompt:`. Overrides
    /// `defaults.role_prompt`; `None` inherits it.
    #[serde(default)]
    pub role_prompt: Option<String>,
    /// ADDITIVE to the composed prompt (§5.6), never replacing.
    #[serde(default)]
    pub prompt: Option<String>,
    /// no DoD judges run after this step ⇒ nothing merges; the work STAGES (§5.7).
    #[serde(default)]
    pub skip_judges: bool,
    /// blast-radius isolation for this step (`none` | `sandbox` | `container`); overrides
    /// `defaults.isolation`. `None` inherits it. See [`crate::isolation::Isolation`].
    #[serde(default)]
    pub isolation: Option<crate::isolation::Isolation>,
    /// the base image for this step under `isolation: container`; overrides `defaults.image`.
    #[serde(default)]
    pub image: Option<String>,
    /// project-relative paths this step may READ but not WRITE — extra denies handed to the OS
    /// wrapper on top of the derived `agg/private/` carve-out.
    ///
    /// ⚠ **Inert without a confining tier.** The deny list is delivered by the wrapper, and the
    /// wrapper only runs under `isolation: sandbox`/`container`; under the default `none` there is
    /// no mechanism to deliver it to and the step can write every path listed. agg warns; it does
    /// not silently pretend.
    #[serde(default)]
    pub readonly: Vec<String>,
    /// paths SUBTRACTED from [`Self::readonly`] — how a step re-grants exactly one of the denies it
    /// would otherwise carry. Matching is exact on the normalised spelling
    /// ([`crate::isolation::normalize_path`]), so `agg/judges` and `agg/judges/` are one path.
    #[serde(default)]
    pub writable: Vec<String>,
}

/// The sequence: a repeating statement list + the run-level ceilings and Definition of Done. The
/// three ceilings (tokens, cost, sessions) are UNIFIED under one `limits:` block (§4.1) — previously
/// separate `budget:`/`cost:`/`max_sessions:` keys.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    /// statement lines (`worker x4`, `if stalled then reconsider`) — parsed by `core::sequence`.
    pub steps: Vec<String>,
    /// the run-level ceilings — tokens (worker AND judge spend), dollars, sessions. Each null/absent
    /// = unlimited. The loop reads these into the `budget_total`/`cost_limit`/`max_sessions` fields
    /// that back the stable `over_budget`/`over_cost`/`over_iterations` grammar.
    #[serde(default)]
    pub limits: Limits,
    /// RENAME of the shipped `session_isolation.rollback_on_regression` (§5.7).
    #[serde(default = "default_true")]
    pub gate_regressions: bool,
    /// judges that must STAY met.
    #[serde(default)]
    pub invariants: Vec<String>,
    /// the Definition of Done — success stop (exit 0). RENAME of `stop_when`.
    #[serde(default = "default_done_if")]
    pub done_if: String,
    /// the giving-up guard (exit 3). RENAME of `halt_when`.
    #[serde(default)]
    pub abort_if: Option<String>,
    /// the NON-TERMINAL twin of `abort_if` (STUCK_NOTIFY §3): same grammar, but firing it runs
    /// `notify.cmd` and the loop KEEPS RUNNING. Absent ⇒ never notify on a live cycle.
    #[serde(default)]
    pub notify_if: Option<String>,
    /// how a notification is delivered. Valid WITHOUT `notify_if` — that is the "stop + notify"
    /// policy (ping only when `abort_if` halts, §8.5). Required WITH it (else nothing would fire).
    #[serde(default)]
    pub notify: Option<NotifyCfg>,
}

/// Delivery for `notify_if` / an `abort_if` halt (STUCK_NOTIFY §5).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyCfg {
    /// shell commands, run exactly like a hook — best-effort, non-fatal, in the step's jail. Each
    /// string may contain `{{reason}}` / `{{project}}` / `{{session}}` / `{{step}}`, which agg
    /// substitutes SHELL-QUOTED (§12.4) — do not add quotes of your own around a placeholder.
    #[serde(default)]
    pub cmd: Vec<String>,
    /// debounce: minimum sessions between two `notify_if` fires. `0` = every qualifying cycle. The
    /// terminal `abort_if` ping ignores this (it happens once, at the end).
    #[serde(default = "default_cooldown")]
    pub cooldown_sessions: u32,
}

/// Per-session git isolation — MANDATORY, no master switch. The keep/rollback decision moved to
/// `sequence.gate_regressions`; only these three keys survive here (§4.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIsolation {
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,
    #[serde(default)]
    pub base_branch: String,
    #[serde(default = "default_red_file")]
    pub red_file: String,
}

impl Default for SessionIsolation {
    fn default() -> Self {
        SessionIsolation {
            branch_prefix: default_branch_prefix(),
            base_branch: String::new(),
            red_file: default_red_file(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    #[serde(default)]
    pub on_start: Vec<String>,
    #[serde(default)]
    pub on_session_start: Vec<String>,
    #[serde(default)]
    pub on_session_end: Vec<String>,
    #[serde(default)]
    pub on_stop: Vec<String>,
    #[serde(default)]
    pub background: Vec<String>,
}

/// LLM-summary settings. `model` is dropped (§4.1) — the summarizer runs on the RULER.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_summary_interval")]
    pub min_interval_secs: u64,
}

impl Default for Summary {
    fn default() -> Self {
        Summary { enabled: default_true(), min_interval_secs: default_summary_interval() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watchdog {
    #[serde(default = "default_wd_idle")]
    pub idle_secs: u64,
    #[serde(default = "default_wd_cpu")]
    pub cpu_grace: u64,
}

impl Default for Watchdog {
    fn default() -> Self {
        Watchdog { idle_secs: default_wd_idle(), cpu_grace: default_wd_cpu() }
    }
}

/// The run-level spend/iteration ceilings, unified (§4.1). Every field null/absent = unlimited. The
/// loop reads these into the StopContext/state.json `budget_total`/`cost_limit`/`max_sessions`
/// fields, which back the stable `over_budget`/`over_cost`/`over_iterations` grammar — the SOURCE of
/// those values moved here; the grammar terms and state.json field names did NOT change.
///
/// ONE struct, shared by both entry points: the YAML path reads it out of `sequence.limits:`, and a
/// Rust driver hands the same value to `Agg::limits(..)`. That is why [`Limits::wall_hours`] is a
/// valid `agg.yaml` key even though only the driver path enforces it — a second, driver-only limits
/// struct would have to be kept in sync with this one forever, for no gain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// output-token ceiling — WORKER *and* JUDGE spend (§5.6). null = unlimited. Backs `over_budget`.
    #[serde(default)]
    pub tokens: Option<u64>,
    /// dollar ceiling. null = unlimited. CLAUDE-only in practice (only it reports dollars). Backs `over_cost`.
    #[serde(default)]
    pub cost: Option<f64>,
    /// session cap. null = unlimited (was the old `max_sessions: 0` sentinel). Backs `over_iterations`.
    /// A non-zero `--max-sessions <n>` flag overrides it when passed (§4.1).
    #[serde(default)]
    pub sessions: Option<u32>,
    /// wall-clock ceiling in hours, measured from the run's start. null = unlimited.
    ///
    /// ADDITIVE (BUILD.md §2.2): every config written before this key existed still parses, because
    /// it is `#[serde(default)]` like its three siblings. It COEXISTS with the `wall_hours`
    /// CONDITION term (`core::stop`) rather than replacing it — both read the same clock, but
    /// `wall_hours >= 8 OR stalled` is not expressible as a limit, so the term stays.
    ///
    /// ⚠ Enforced only where a driver calls `Agg::check_limits()`. The YAML path keeps using the
    /// `wall_hours` term in `abort_if`.
    ///
    /// ⚠ Known ceiling: the run clock is an `Instant`, so a RESUMED run restarts it and gets the
    /// full allowance again.
    /// `ponytail:` upgrade path — seed the clock from `RunRecord.started_at_epoch` (Phase 4).
    #[serde(default)]
    pub wall_hours: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_memory_max_kb")]
    pub max_kb: Option<u64>,
    #[serde(default = "default_memory_inject_kb")]
    pub inject_kb: Option<u64>,
}

impl Default for Memory {
    fn default() -> Self {
        Memory {
            enabled: default_true(),
            max_kb: default_memory_max_kb(),
            inject_kb: default_memory_inject_kb(),
        }
    }
}

// ---- defaults ----
fn default_agent() -> String {
    "claude".into()
}
fn default_state() -> String {
    "state/STATE.md".into()
}
fn default_branch_prefix() -> String {
    "agg".into()
}
fn default_red_file() -> String {
    ".agg_red".into()
}
fn default_heartbeat() -> u64 {
    30
}
fn default_backoff() -> u64 {
    1800
}
fn default_wd_idle() -> u64 {
    900
}
fn default_wd_cpu() -> u64 {
    180
}
fn default_done_if() -> String {
    "all_goals".into()
}
fn default_cooldown() -> u32 {
    3
}
fn default_true() -> bool {
    true
}
fn default_judge_timeout() -> u64 {
    300
}
fn default_summary_interval() -> u64 {
    300
}
fn default_memory_max_kb() -> Option<u64> {
    Some(64)
}
fn default_memory_inject_kb() -> Option<u64> {
    Some(8)
}

/// An `Arc<dyn AgentBackend>` that is `Debug`.
///
/// The trait deliberately does NOT require `Debug` — that would be a tax on every backend author
/// for one line of formatting — so a bare `Arc<dyn AgentBackend>` field would kill [`ResolvedStep`]'s
/// derive and force a hand-written 15-field `Debug` that a future field addition would silently
/// skip. One newtype with one `Debug` impl is cheaper and stays correct.
#[derive(Clone)]
pub struct CustomBackend(pub Arc<dyn AgentBackend>);

impl std::fmt::Debug for CustomBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CustomBackend({})", self.0.name())
    }
}

/// A step body merged over [`Defaults`] — everything one worker session needs, resolved. The loop
/// builds one per step at session-build time (§5.5). Agent/model/effort resolve against the STEP's
/// backend, not a process-wide one.
#[derive(Debug, Clone)]
pub struct ResolvedStep {
    pub name: String,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub worker_args: Vec<String>,
    pub state: String,
    pub role_prompt: Option<String>,
    pub prompt: Option<String>,
    pub skip_judges: bool,
    /// resolved blast-radius isolation (step over defaults, default none) — [`crate::isolation`].
    pub isolation: crate::isolation::Isolation,
    /// the base image for `isolation: container` (step over defaults, else
    /// [`crate::isolation::DEFAULT_IMAGE`]). Inert on every other tier.
    pub image: String,
    /// paths this step may read but not write — normalised, and already accumulated down its
    /// template chain on the driver path. See [`StepBody::readonly`].
    pub readonly: Vec<String>,
    /// paths subtracted from [`Self::readonly`]. See [`StepBody::writable`]; [`Self::denied`] is the
    /// resulting deny set.
    pub writable: Vec<String>,
    /// a backend the DRIVER implements itself (`Agent::Custom`), consulted by [`Self::backend`]
    /// BEFORE `backend::for_name`. Always `None` on the YAML path — `agent:` is a string there and
    /// an `Arc<dyn AgentBackend>` cannot be `Deserialize`, which is the documented break in the
    /// "one struct, two constructors" claim (BUILD.md §3.8).
    pub custom: Option<CustomBackend>,
}

impl ResolvedStep {
    /// The backend this step runs on — the driver's own [`CustomBackend`] first, else the named one.
    ///
    /// ⚠ The returned reference borrows `self` rather than being `&'static`: a custom backend lives
    /// in the step, not in the binary's static data. Every shipped backend still coerces in.
    pub fn backend(&self) -> Result<&dyn AgentBackend> {
        match &self.custom {
            Some(b) => Ok(&*b.0),
            None => for_name(&self.agent),
        }
    }

    /// [`Self::backend`] narrowed to `&'static` — for the ONE call site that hands the backend to
    /// the worker's stream-reader thread (`worker::run_session`, whose `move` closure needs it to
    /// outlive the session).
    ///
    /// ponytail: a [`CustomBackend`] lives in an `Arc`, not in static data, so it cannot satisfy
    /// that bound and this REFUSES loudly rather than quietly running the named agent instead. The
    /// upgrade path is `run_session`/`spawn_reader` taking `Arc<dyn AgentBackend>` (the trait is
    /// already `Send + Sync`, and a cloned `Arc` into the thread costs one refcount) — commit 6
    /// makes that change, when there is finally a custom backend to run.
    pub fn static_backend(&self) -> Result<&'static dyn AgentBackend> {
        if let Some(b) = &self.custom {
            anyhow::bail!(
                "step `{}` names a driver-supplied backend (`{}`), which the worker launch cannot \
                 run yet — see `ResolvedStep::static_backend`",
                self.name,
                b.0.name()
            );
        }
        for_name(&self.agent)
    }

    /// The paths this step may NOT write: `readonly` minus `writable`
    /// ([`crate::isolation::denied_paths`]). Handed to the wrapper as a SECOND argument beside its
    /// derived `agg/private/` carve-out — never as a replacement for it.
    pub fn denied(&self) -> Vec<String> {
        crate::isolation::denied_paths(&self.readonly, &self.writable)
    }
    /// The worker model, resolved against the step's backend. Absent = the backend's own default.
    pub fn model<'a>(&'a self, b: &dyn AgentBackend) -> &'a str {
        self.model.as_deref().unwrap_or_else(|| b.default_model())
    }
    /// The worker effort, same resolution. Empty = pass no effort flag.
    pub fn effort<'a>(&'a self, b: &dyn AgentBackend) -> &'a str {
        self.effort.as_deref().unwrap_or_else(|| b.default_effort())
    }
}

#[cfg(test)]
mod tests;
