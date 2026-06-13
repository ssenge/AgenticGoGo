//! Configuration loading: `agg.yaml` (harness) and `goals.yaml` (goals + stop).
//!
//! Both are plain YAML. Env vars (`AGG_*`) override a few hot knobs for CI.

use crate::model::Goal;
use anyhow::{Context, Result};
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
    pub summary: Summary,
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
}

impl Default for SessionIsolation {
    fn default() -> Self {
        Self {
            enabled: false,
            branch_prefix: default_branch_prefix(),
            base_branch: String::new(),
            red_file: default_red_file(),
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
    "claude-opus-4-8[1m]".into()
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
    "haiku".into()
}
fn default_summary_interval() -> u64 {
    300
}

impl AggConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading agg config {}", path.display()))?;
        let mut cfg: AggConfig =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
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
    }
}

impl GoalsConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading goals {}", path.display()))?;
        let cfg: GoalsConfig =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        anyhow::ensure!(!cfg.goals.is_empty(), "goals.yaml has no goals");
        Ok(cfg)
    }
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.parse().ok()
}
