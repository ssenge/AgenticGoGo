//! Configuration loading: `agg.yaml` (harness) and `goals.yaml` (goals + stop).
//!
//! Both are plain YAML. Env vars (`AGG_*`) override a few hot knobs for CI.

use crate::core::model::Goal;
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

/// Harness configuration (`agg.yaml`).
#[derive(Debug, Clone, Deserialize)]
pub struct AggConfig {
    pub project: String,
    /// model id for the inner worker
    #[serde(default = "default_model")]
    pub model: String,
    /// `--effort` level passed to each headless worker (`claude -p`). Valid CLI
    /// values: low | medium | high | xhigh | max. Defaults to `max` — the top of
    /// the `-p` flag enum (the interactive-only `ultracode` tier is NOT reachable
    /// from `-p`; workers opt into multi-agent orchestration via the prompt instead,
    /// see `worker_prompt_prefix`). An unrecognized value makes the CLI fall back to
    /// its default effort (with a warning), so keep it to the valid set.
    #[serde(default = "default_effort")]
    pub effort: String,
    /// path to the fat resume prompt fed to each worker (`-p` argument)
    pub resume_prompt: String,
    #[serde(default = "default_heartbeat")]
    pub heartbeat_secs: u64,
    #[serde(default)]
    pub watchdog: Watchdog,
    #[serde(default = "default_backoff")]
    pub ratelimit_backoff_secs: u64,
    #[serde(default)]
    pub budget: Budget,
    #[serde(default)]
    pub cost: Cost,
    #[serde(default)]
    pub summary: Summary,
    /// Institutional memory (#3) — durable cross-session learnings. See [`Memory`].
    #[serde(default)]
    pub memory: Memory,
    /// Continue each session from the previous one's context (`--resume`) instead of
    /// a fresh context. DEFAULT false: fresh-context-per-session is the core discipline
    /// (no context accumulation = no runaway cost). Enable only for short, tightly-scoped
    /// runs where carrying context across sessions genuinely helps and won't balloon.
    #[serde(default)]
    pub resume_sessions: bool,
    /// Generic lifecycle hooks — shell commands agg runs at defined moments. TOOL-AGNOSTIC:
    /// agg knows nothing about what they do. Use them to wire in YOUR tools (a code-graph
    /// builder, a memory-cache refresh, a linter, …). See [`Hooks`].
    #[serde(default)]
    pub hooks: Hooks,
    /// Files whose contents are prepended to every worker prompt (after any operator
    /// instruction, before the resume prompt). Compose reusable tooling/guidance fragments
    /// here instead of baking them into agg. Paths are relative to the project dir.
    #[serde(default)]
    pub prompt_includes: Vec<String>,
    /// Extra flags appended to every worker `claude -p` invocation. The worker ALWAYS runs with
    /// `--dangerously-skip-permissions` (a headless `-p` worker cannot answer permission prompts,
    /// so it needs full host access — see the README "What the worker can do"). Use this to
    /// constrain it: e.g. `worker_args: ["--allowedTools", "Edit,Bash", "--add-dir", "src"]`, or
    /// to add any other `claude` flag agg doesn't manage. Applied after agg's own flags, before
    /// `-p <prompt>`. Empty by default.
    #[serde(default)]
    pub worker_args: Vec<String>,
    /// Per-session git branch isolation. When enabled, each worker session runs on its own
    /// branch and is merged back to the base ONLY if the worker did not veto it. See
    /// [`SessionIsolation`].
    #[serde(default)]
    pub session_isolation: SessionIsolation,
}

/// Per-session git isolation. Each session runs on `<branch_prefix>/<project>/session-<N>`
/// branched from `base_branch`. After the session, agg merges that branch back into the base
/// — DEFAULT = merge — UNLESS the worker wrote the `red_file` (a veto), in which case the
/// session branch is discarded and the base is left untouched. A crashed/killed worker that
/// never wrote the red file still merges its partial commits (default-merge). The worker is
/// the authority on green/red; agg only acts on the veto file.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionIsolation {
    /// master switch (default OFF — existing projects keep committing to the current branch).
    #[serde(default)]
    pub enabled: bool,
    /// branch name prefix; full name is `<prefix>/<project>/session-<N>`.
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,
    /// base branch sessions are cut from + merged into. Empty = whatever branch agg was
    /// launched on (captured at startup).
    #[serde(default)]
    pub base_branch: String,
    /// the worker's veto file (relative to project dir). Present after a session ⇒ DO NOT
    /// merge (discard the session branch). agg deletes it before each session so a stale
    /// veto never blocks a later merge.
    #[serde(default = "default_red_file")]
    pub red_file: String,
    /// ROLLBACK GATE (#11): stage the session's merge, re-run the judges against the merged tree,
    /// and ROLL BACK the merge if a previously-met goal regressed because of it (base stays put,
    /// the branch is kept for inspection). DEFAULT on when isolation is on — it can only prevent a
    /// known-bad merge from landing. A judge that merely *couldn't run* (timeout/spawn-fail/
    /// rate-limit/bad-JSON) never triggers rollback — only a real regression does.
    #[serde(default = "default_true")]
    pub rollback_on_regression: bool,
}

impl Default for SessionIsolation {
    fn default() -> Self {
        Self {
            enabled: false,
            branch_prefix: default_branch_prefix(),
            base_branch: String::new(),
            red_file: default_red_file(),
            rollback_on_regression: default_true(),
        }
    }
}

/// Generic lifecycle hooks. Each is a list of shell commands run (in order) at that moment,
/// from the project dir. agg is tool-agnostic — these are whatever YOU put in them.
///
/// `background` commands are long-lived (e.g. a file watcher): agg spawns them at loop start
/// in the worker's reaping domain and they are cleaned up by the straggler reaper, so a
/// `--watch`-style process can't leak.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Hooks {
    /// run once, at loop startup (before the first session). e.g. build a code graph.
    #[serde(default)]
    pub on_start: Vec<String>,
    /// run before each worker session. e.g. incremental refresh of a cache/graph.
    #[serde(default)]
    pub on_session_start: Vec<String>,
    /// run after each session's judging. e.g. persist a memory note, update an index.
    #[serde(default)]
    pub on_session_end: Vec<String>,
    /// run once, when the loop stops (success/halt/bus-stop). e.g. teardown, final export.
    #[serde(default)]
    pub on_stop: Vec<String>,
    /// long-lived commands spawned at loop start (e.g. a `--watch`). Reaped on stop.
    #[serde(default)]
    pub background: Vec<String>,
}

/// LLM-summary settings. After each cycle (or every N secs), a cheap
/// model condenses recent worker thoughts + goal deltas into a cumulative + a
/// windowed one-liner.
#[derive(Debug, Clone, Deserialize)]
pub struct Summary {
    /// master switch
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// cheap model for the summarizer (haiku by default)
    #[serde(default = "default_summary_model")]
    pub model: String,
    /// minimum seconds between summaries (rate-limit the summarizer itself)
    #[serde(default = "default_summary_interval")]
    pub min_interval_secs: u64,
}

impl Default for Summary {
    fn default() -> Self {
        Summary {
            enabled: default_true(),
            model: default_summary_model(),
            min_interval_secs: default_summary_interval(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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
pub struct Budget {
    /// total output-token ceiling; `None` = unlimited
    #[serde(default)]
    pub total: Option<u64>,
}

/// Dollar-spend ceiling (`cost.total`). Distinct from [`Budget`] (tokens). We don't price
/// anything ourselves — Claude reports `total_cost_usd` on each session's result event
/// (correctly per-model, `[1m]`-variant- and cache-aware) and we just sum it. The `over_cost`
/// stop term trips once the sum exceeds `total`.
///
/// NOTE: `total_cost_usd` is the API-EQUIVALENT list price of the work, not necessarily money
/// billed. On a Max/Pro **subscription** the user is not charged per token, so this is a usage
/// proxy — the dashboard/`agg status` label it `(API-eq)`. It's still a valid runaway ceiling
/// (relative spend); users wanting a plan-agnostic cap should use `over_budget`/`over_iterations`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Cost {
    /// total dollar ceiling; `None` = unlimited
    #[serde(default)]
    pub total: Option<f64>,
}

/// Institutional-memory settings (#3). agg maintains a durable `AGG_MEMORY.md` at the project
/// root (rolled-up learnings) and injects a BOUNDED slice of it + a last-session block into every
/// worker prompt. ENFORCED — agg writes memory itself even if the worker crashes / is killed /
/// ignores it. Two independent caps: `max_kb` bounds the file on disk; `inject_kb` bounds how
/// much is injected per prompt (token-cost control — a large audit file must not balloon prompts).
#[derive(Debug, Clone, Deserialize)]
pub struct Memory {
    /// master switch (DEFAULT on — memory is a core continuity feature).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// on-disk cap for `AGG_MEMORY.md`; when exceeded the OLDEST entries drop first.
    /// `None` = no cap. `AGG_MEMORY_MAX_KB` overrides (0 ⇒ uncapped).
    #[serde(default = "default_memory_max_kb")]
    pub max_kb: Option<u64>,
    /// READ-side cap: only the NEWEST `inject_kb` of the durable file is injected into each
    /// prompt, independent of `max_kb`. Bounds per-prompt input tokens. `None` = inject all
    /// (NOT recommended). `AGG_MEMORY_INJECT_KB` overrides (0 ⇒ inject all).
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

/// Goals file (`goals.yaml`): the goal list + stop/halt conditions.
#[derive(Debug, Clone, Deserialize)]
pub struct GoalsConfig {
    pub goals: Vec<Goal>,
    /// expression that, when true, stops the loop (success). Default: all goals met.
    #[serde(default = "default_stop")]
    pub stop_when: String,
    /// expression that, when true, halts immediately (failure/guard). Optional.
    #[serde(default)]
    pub halt_when: Option<String>,
}

// ---- defaults ----
fn default_model() -> String {
    crate::backend::DEFAULT_MODEL.into()
}
fn default_effort() -> String {
    "max".into()
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
fn default_stop() -> String {
    "all_goals".into()
}
fn default_true() -> bool {
    true
}
fn default_summary_model() -> String {
    crate::backend::DEFAULT_SUMMARY_MODEL.into()
}
fn default_summary_interval() -> u64 {
    300
}
fn default_memory_max_kb() -> Option<u64> {
    Some(64) // 64 KB on disk — generous for rolled-up learnings, bounded so it can't balloon.
}
fn default_memory_inject_kb() -> Option<u64> {
    Some(8) // 8 KB into each prompt (~2k tokens) — keeps memory from undermining the budget.
}

impl AggConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let mut cfg: AggConfig = crate::util::load_yaml(path)?;
        cfg.apply_env_overrides();
        Ok(cfg)
    }

    /// CI-friendly env overrides for the hot knobs.
    fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("AGG_MODEL") {
            self.model = v;
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
        // CI safety knob: clamp the dollar ceiling without editing agg.yaml.
        if let Some(v) = env_f64("AGG_COST_TOTAL") {
            self.cost.total = Some(v);
        }
        // memory caps (CI / quick experiments): 0 ⇒ uncapped/inject-all.
        if let Some(v) = env_u64("AGG_MEMORY_MAX_KB") {
            self.memory.max_kb = if v == 0 { None } else { Some(v) };
        }
        if let Some(v) = env_u64("AGG_MEMORY_INJECT_KB") {
            self.memory.inject_kb = if v == 0 { None } else { Some(v) };
        }
    }
}

impl GoalsConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let cfg: GoalsConfig = crate::util::load_yaml(path)?;
        anyhow::ensure!(!cfg.goals.is_empty(), "goals.yaml has no goals");
        Ok(cfg)
    }
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.parse().ok()
}

fn env_f64(key: &str) -> Option<f64> {
    std::env::var(key).ok()?.parse().ok()
}
