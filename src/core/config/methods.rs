//! `impl AggConfig` behaviour: the ruler/worker backend resolution, the per-step merge into a
//! [`ResolvedStep`], the agent-name collection, `load` + its CI-friendly env overrides, plus the
//! partial-parse agent reader and the `env_u64`/`env_f64` parse helpers. Pure behaviour — the serde
//! shape and the `default_*` fns live in the parent module.

use super::*;
use anyhow::anyhow;
use std::path::Path;

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
            role_prompt: body.role_prompt.clone().or_else(|| self.defaults.role_prompt.clone()),
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
    /// `AGG_COST_TOTAL` → sequence.limits.cost, `AGG_TOKEN_BUDGET` → sequence.limits.tokens.
    pub(super) fn apply_env_overrides(&mut self) {
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
            self.sequence.limits.cost = Some(v);
        }
        if let Some(v) = env_u64("AGG_TOKEN_BUDGET") {
            self.sequence.limits.tokens = Some(v);
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
