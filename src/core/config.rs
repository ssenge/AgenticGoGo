//! Configuration loading: `agg.yaml` (harness + steps + sequence). One file now — `goals.yaml`
//! is DELETED (§7.1): a judge IS a goal, resolved by name from disk.
//!
//! `#[serde(deny_unknown_fields)]` is on EVERY struct here (§4.1): without it a stale top-level
//! `budget:` after the config move is silently ignored — an autonomous loop whose spend ceiling is
//! a decorative key. That guard is also what makes "any other key in a step body (esp `judge_*`) is
//! a HARD ERROR" true.

use crate::backend::{for_name, AgentBackend};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

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
    /// the forward state file (`AGG_STATE.md`), resolved against the project dir; the AGENT writes
    /// it best-effort (§5.6). Renamed from the old `resume_prompt`.
    #[serde(default = "default_state")]
    pub state: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            agent: default_agent(),
            model: None,
            effort: None,
            worker_args: vec![],
            state: default_state(),
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

/// One step's body — a bag of OVERRIDES over [`Defaults`], plus `prompt`/`skip_judges`. The
/// COMPLETE legal key list (§4.1); any other key is a HARD ERROR (deny_unknown), which is what
/// makes naming a `judge_*` key in a step fail loudly.
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
    /// ADDITIVE to the composed prompt (§5.6), never replacing.
    #[serde(default)]
    pub prompt: Option<String>,
    /// no DoD judges run after this step ⇒ nothing merges; the work STAGES (§5.7).
    #[serde(default)]
    pub skip_judges: bool,
}

/// The sequence: a repeating statement list + the run-level ceilings and Definition of Done. Budget,
/// cost and max_sessions MOVED here from top level / the CLI (§4.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    /// statement lines (`worker x4`, `if stalled then reconsider`) — parsed by `core::sequence`.
    pub steps: Vec<String>,
    /// output-token ceiling — WORKER *and* JUDGE spend (§5.6). MOVED from top level.
    #[serde(default)]
    pub budget: Budget,
    /// dollar ceiling. MOVED from top level.
    #[serde(default)]
    pub cost: Cost,
    /// 0 = unlimited. Backs `over_iterations`. MOVED from the `--max-sessions` CLI flag (which
    /// survives and WINS when passed — §4.1).
    #[serde(default)]
    pub max_sessions: u32,
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    #[serde(default)]
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cost {
    #[serde(default)]
    pub total: Option<f64>,
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
    "AGG_STATE.md".into()
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
    pub prompt: Option<String>,
    pub skip_judges: bool,
}

impl ResolvedStep {
    /// The backend this step runs on.
    pub fn backend(&self) -> Result<&'static dyn AgentBackend> {
        for_name(&self.agent)
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

impl AggConfig {
    /// The **RULER**: the backend that runs the LLM judges and the summarizer. Reads `judge.agent`.
    pub fn ruler_backend(&self) -> Result<&'static dyn AgentBackend> {
        for_name(&self.judge.agent)
    }

    /// The default WORKER backend (`defaults.agent`) — for the banner/dashboard and for the paths
    /// that want "the primary agent" without a specific step. Per-step, use [`Self::resolve_step`].
    pub fn worker_backend(&self) -> Result<&'static dyn AgentBackend> {
        for_name(&self.defaults.agent)
    }

    /// The judge model (`judge.model`), resolved against the ruler. Absent = the ruler's cheap default.
    pub fn judge_model(&self, ruler: &dyn AgentBackend) -> String {
        self.judge.model.clone().unwrap_or_else(|| ruler.default_summary_model().to_string())
    }

    /// Merge a named step's body over [`Defaults`] into a [`ResolvedStep`]. Errors (listing the
    /// palette) if the name is not a key in `steps:` — a startup hard error, never a runtime surprise.
    pub fn resolve_step(&self, name: &str) -> Result<ResolvedStep> {
        let body = self.steps.get(name).ok_or_else(|| {
            let names: Vec<&str> = self.steps.keys().map(String::as_str).collect();
            anyhow!("unknown step `{name}` — not a key in `steps:`. defined: {}", names.join(", "))
        })?;
        Ok(ResolvedStep {
            name: name.to_string(),
            agent: body.agent.clone().unwrap_or_else(|| self.defaults.agent.clone()),
            model: body.model.clone().or_else(|| self.defaults.model.clone()),
            effort: body.effort.clone().or_else(|| self.defaults.effort.clone()),
            worker_args: body.worker_args.clone().unwrap_or_else(|| self.defaults.worker_args.clone()),
            state: body.state.clone().unwrap_or_else(|| self.defaults.state.clone()),
            prompt: body.prompt.clone(),
            skip_judges: body.skip_judges,
        })
    }

    /// Every distinct agent named anywhere (defaults + judge + each step) — so `agg doctor` and the
    /// capability check can cover EVERY agent the sequence names (§7.3), not just one.
    pub fn agent_names(&self) -> Vec<String> {
        let mut names = vec![self.defaults.agent.clone(), self.judge.agent.clone()];
        for body in self.steps.values() {
            if let Some(a) = &body.agent {
                names.push(a.clone());
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// Read ONLY the worker agent (`defaults.agent`), tolerating a missing / unparseable / partial
    /// config — for `doctor` / `skills install`, which must name an agent when agg.yaml may be
    /// absent or broken. Missing → `"claude"`.
    pub fn agent_name(path: &Path) -> String {
        read_agent(path, false)
    }

    /// Read ONLY the ruler agent (`judge.agent`, else `defaults.agent`) — for `plan` / `judge`,
    /// which run judges on the RULER off agg.yaml alone.
    pub fn ruler_name(path: &Path) -> String {
        read_agent(path, true)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let mut cfg: AggConfig = crate::util::load_yaml(path)?;
        cfg.apply_env_overrides();
        Ok(cfg)
    }

    /// CI-friendly env overrides, re-homed under the new shape (§4.1). `AGG_MODEL` → defaults.model,
    /// `AGG_COST_TOTAL` → sequence.cost.total, the rest unchanged.
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("AGG_MODEL") {
            self.defaults.model = Some(v);
        }
        if let Some(v) = env_u64("AGG_HEARTBEAT_SECS") {
            self.heartbeat_secs = v;
        }
        if let Some(v) = env_u64("AGG_WATCHDOG_IDLE_SECS") {
            self.watchdog.idle_secs = v;
        }
        if let Some(v) = env_u64("AGG_WATCHDOG_CPU_GRACE") {
            self.watchdog.cpu_grace = v;
        }
        if let Some(v) = env_u64("AGG_RATELIMIT_BACKOFF") {
            self.ratelimit_backoff_secs = v;
        }
        if let Some(v) = env_f64("AGG_COST_TOTAL") {
            self.sequence.cost.total = Some(v);
        }
        if let Some(v) = env_u64("AGG_MEMORY_MAX_KB") {
            self.memory.max_kb = if v == 0 { None } else { Some(v) };
        }
        if let Some(v) = env_u64("AGG_MEMORY_INJECT_KB") {
            self.memory.inject_kb = if v == 0 { None } else { Some(v) };
        }
    }
}

/// Partial-parse the agent name(s) from agg.yaml without the deny_unknown_fields full parse, so it
/// survives a config that is missing / broken / half-written. `ruler` picks `judge.agent`.
fn read_agent(path: &Path, ruler: bool) -> String {
    #[derive(Deserialize, Default)]
    struct AgentOnly {
        agent: Option<String>,
    }
    #[derive(Deserialize, Default)]
    struct Partial {
        #[serde(default)]
        defaults: AgentOnly,
        #[serde(default)]
        judge: AgentOnly,
    }
    let parsed: Option<Partial> =
        std::fs::read_to_string(path).ok().and_then(|t| serde_yaml::from_str(&t).ok());
    let p = parsed.unwrap_or_default();
    if ruler {
        p.judge.agent.or(p.defaults.agent)
    } else {
        p.defaults.agent
    }
    .unwrap_or_else(default_agent)
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.parse().ok()
}

fn env_f64(key: &str) -> Option<f64> {
    std::env::var(key).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a config body the way `AggConfig::load` does (minus the env overrides).
    fn parse(body: &str) -> Result<AggConfig, serde_yaml::Error> {
        serde_yaml::from_str::<AggConfig>(body)
    }

    /// The smallest config that parses — `project` + `sequence` are the only required keys.
    const MINIMAL: &str = "project: p\nsteps: { worker: {} }\nsequence: { steps: [worker] }\n";

    #[test]
    fn the_minimal_config_parses_with_all_defaults() {
        let cfg = parse(MINIMAL).expect("minimal config parses");
        assert_eq!(cfg.project, "p");
        assert_eq!(cfg.defaults.agent, "claude");
        assert_eq!(cfg.judge.timeout, 300);
        assert_eq!(cfg.sequence.done_if, "all_goals");
        assert!(cfg.sequence.gate_regressions, "gate_regressions defaults ON (the rename default)");
    }

    /// §4.1: `budget`/`cost` MOVED under `sequence:`; a stale top-level `budget:` after the move
    /// would be a decorative spend ceiling — an unbounded loop. `deny_unknown_fields` makes it a HARD
    /// ERROR instead of a silent no-op. This is THE guard the config move depends on.
    #[test]
    fn a_stray_top_level_budget_is_a_hard_error_not_silently_ignored() {
        let err = parse(&format!("{MINIMAL}budget: {{ total: 5 }}\n")).unwrap_err().to_string();
        assert!(err.contains("unknown field `budget`"), "must reject the moved key, got: {err}");
        // and the SAME key is legal in its new home, under `sequence:`.
        parse("project: p\nsteps: { worker: {} }\nsequence: { steps: [worker], budget: { total: 5 } }\n")
            .expect("`budget` under `sequence:` is its new home and must parse");
    }

    /// §4.1: the RULER block is immutable; naming any `judge*` key (or any non-[`StepBody`] key) in a
    /// step body is a HARD ERROR — a grader that moves makes verdicts incomparable across cycles.
    #[test]
    fn a_judge_key_in_a_step_body_is_a_hard_error() {
        let err = parse("project: p\nsteps: { worker: { judge: mine } }\nsequence: { steps: [worker] }\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown field `judge`"), "a step may not name a judge, got: {err}");
        // a stray recheck-era key in a step body is likewise refused (deny_unknown catches all of them).
        let err2 = parse("project: p\nsteps: { worker: { recheck: once_met } }\nsequence: { steps: [worker] }\n")
            .unwrap_err()
            .to_string();
        assert!(err2.contains("unknown field `recheck`"), "got: {err2}");
    }

    /// §4.1/§7.3: `resume_sessions` is refused unconditionally — a per-agent session id cannot cross
    /// a mixed sequence, so the key is rejected at PARSE time (there is no field for it anywhere).
    #[test]
    fn resume_sessions_is_refused_unconditionally() {
        // top level
        let top = parse(&format!("{MINIMAL}resume_sessions: [1, 2]\n")).unwrap_err().to_string();
        assert!(top.contains("unknown field `resume_sessions`"), "got: {top}");
        // and inside a step body
        let step =
            parse("project: p\nsteps: { worker: { resume_sessions: [1] } }\nsequence: { steps: [worker] }\n")
                .unwrap_err()
                .to_string();
        assert!(step.contains("unknown field `resume_sessions`"), "got: {step}");
    }

    /// A step body overrides the keys it names and INHERITS the rest from `defaults:` (§4). Proves
    /// the per-step agent override resolves, and that an un-named key falls through.
    #[test]
    fn a_step_overrides_what_it_names_and_inherits_the_rest() {
        let cfg = parse(
            "project: p\n\
             defaults: { agent: claude, model: opus, effort: high, worker_args: [\"--sandbox\"], state: S.md }\n\
             steps:\n  plan: {}\n  build: { agent: codex, model: gpt, skip_judges: true }\n\
             sequence: { steps: [plan, build] }\n",
        )
        .expect("config parses");

        // `plan` names nothing → pure defaults.
        let plan = cfg.resolve_step("plan").unwrap();
        assert_eq!(plan.agent, "claude");
        assert_eq!(plan.model.as_deref(), Some("opus"));
        assert_eq!(plan.effort.as_deref(), Some("high"));
        assert_eq!(plan.worker_args, vec!["--sandbox".to_string()]);
        assert_eq!(plan.state, "S.md");
        assert!(!plan.skip_judges);

        // `build` overrides agent + model + skip_judges; effort/worker_args/state inherit.
        let build = cfg.resolve_step("build").unwrap();
        assert_eq!(build.agent, "codex", "the per-step agent override resolves");
        assert_eq!(build.model.as_deref(), Some("gpt"));
        assert_eq!(build.effort.as_deref(), Some("high"), "effort inherits from defaults");
        assert_eq!(build.worker_args, vec!["--sandbox".to_string()], "worker_args inherits");
        assert!(build.skip_judges);

        // an unknown step name is a hard error that lists the palette (never a runtime surprise).
        let err = cfg.resolve_step("nope").unwrap_err().to_string();
        assert!(err.contains("unknown step `nope`") && err.contains("plan") && err.contains("build"), "got: {err}");
    }

    /// `agent_names` returns EVERY distinct agent the sequence names (defaults + ruler + per-step),
    /// sorted and de-duped — so `doctor`/capability can cover them all (§7.3).
    #[test]
    fn agent_names_collects_every_distinct_agent() {
        let cfg = parse(
            "project: p\ndefaults: { agent: claude }\njudge: { agent: claude }\n\
             steps:\n  plan: {}\n  build: { agent: codex }\n  review: { agent: copilot }\n\
             sequence: { steps: [plan, build, review] }\n",
        )
        .unwrap();
        assert_eq!(cfg.agent_names(), vec!["claude", "codex", "copilot"]);
    }

    /// The env overrides re-home onto the new shape (§4.1): `AGG_MODEL` → `defaults.model`,
    /// `AGG_COST_TOTAL` → `sequence.cost.total`. (Serial: mutates process env.)
    #[test]
    fn env_overrides_land_on_the_new_shape() {
        // guard against parallel env races by scoping tightly and restoring.
        std::env::set_var("AGG_MODEL", "haiku-from-env");
        std::env::set_var("AGG_COST_TOTAL", "12.5");
        let mut cfg = parse(MINIMAL).unwrap();
        cfg.apply_env_overrides();
        std::env::remove_var("AGG_MODEL");
        std::env::remove_var("AGG_COST_TOTAL");
        assert_eq!(cfg.defaults.model.as_deref(), Some("haiku-from-env"));
        assert_eq!(cfg.sequence.cost.total, Some(12.5));
    }
}
