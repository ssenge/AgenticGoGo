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
}

/// LLM-summary settings (Phase 3). After each cycle (or every N secs), a cheap
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

    /// Goal ids tagged `invariant: true`.
    #[allow(dead_code)]
    pub fn invariant_ids(&self) -> Vec<String> {
        self.goals.iter().filter(|g| g.invariant).map(|g| g.id.clone()).collect()
    }
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.parse().ok()
}
