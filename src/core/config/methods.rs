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
            // Copy type → no clone. step overrides defaults; both absent ⇒ Isolation::None.
            isolation: body.isolation.or(self.defaults.isolation).unwrap_or_default(),
            image: body
                .image
                .clone()
                .or_else(|| self.defaults.image.clone())
                .unwrap_or_else(|| crate::isolation::DEFAULT_IMAGE.to_string()),
            // Normalised HERE, on the way in, exactly as the driver's builder does it — `writable`
            // subtracts by string, so `writable: [agg/judges]` against `readonly: [agg/judges/]`
            // would otherwise subtract nothing while reading like it worked. There is no
            // `defaults.readonly`: the YAML path has one defaults block and repeating the list per
            // step is the shape `sample_workflow.yaml` documents (the Rust path's templates are the
            // answer to that repetition).
            readonly: crate::isolation::normalize_paths(&body.readonly),
            writable: crate::isolation::normalize_paths(&body.writable),
            // YAML cannot name a driver-supplied backend; `agent:` is a string there.
            custom: None,
        })
    }

    /// The RUN-LEVEL blast-radius tier — `Sandbox` if the run could confine ANY worker (defaults or
    /// some step names `sandbox`), else `None`. Used to confine the run-level teardown hook
    /// (`on_stop`), which fires once, after all workers, with no single step's context (ISOLATION.md
    /// §13). Over-approximates deliberately: confining a best-effort teardown hook that maybe didn't
    /// need it is harmless; leaving it unconfined when a sandboxed worker ran is the escape.
    ///
    /// `container` is deliberately NOT folded in here: judges and hooks are host tooling (agg's own
    /// DoD judges run `cargo`), so re-hosting them in the worker's base image would break them, and
    /// jailing them in the OS sandbox instead is a different policy than the step asked for. The
    /// residual — a container worker can still rewrite a judge/hook script in its bind-mounted cwd —
    /// is documented in `internal/ISOLATION.md` §15, not silently papered over.
    pub fn run_isolation(&self) -> crate::isolation::Isolation {
        use crate::isolation::Isolation;
        let any_step = self.steps.values().any(|b| b.isolation == Some(Isolation::Sandbox));
        if self.defaults.isolation == Some(Isolation::Sandbox) || any_step {
            Isolation::Sandbox
        } else {
            Isolation::None
        }
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
        // BEFORE serde, because `deny_unknown_fields` would report `wall_hours` as merely unknown
        // and the reader would "fix" it by renaming — which is the one migration that silently
        // breaks. Checked on the RAW text so it catches the key AND the condition term in one pass.
        Self::reject_removed_keys(path)?;
        let mut cfg: AggConfig = crate::util::load_yaml(path)?;
        cfg.apply_env_overrides();
        cfg.warn_suspicious_clock_bounds();
        Ok(cfg)
    }

    /// A hard error naming the replacement AND doing the arithmetic.
    ///
    /// `wall_hours` became `wall_time`, in **seconds** — a 3600x unit change. serde's
    /// `deny_unknown_fields` catches `limits.wall_hours`, but its message ("unknown field") invites
    /// exactly the wrong repair, and it cannot see `abort_if: "wall_hours >= 8"` at all because that
    /// is a string. Both surfaces are caught here, in the raw YAML, and the message converts the
    /// value for the reader. See `internal/HUMAN_LOOP.md` §7.4.1.
    fn reject_removed_keys(path: &Path) -> Result<()> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Ok(()), // a missing/unreadable file is `load_yaml`'s error to report
        };
        for (n, line) in text.lines().enumerate() {
            // Comment-aware: a config that DOCUMENTS the migration must still load.
            let code = line.split('#').next().unwrap_or("");
            if !code.contains("wall_hours") {
                continue;
            }
            let hint = code
                .split(|c: char| !c.is_ascii_digit() && c != '.')
                .filter(|t| !t.is_empty())
                .find_map(|t| t.parse::<f64>().ok())
                .map(|h| format!("\n    you wrote {h} — in seconds that is {}", (h * 3600.0) as u64))
                .unwrap_or_default();
            anyhow::bail!(
                "{}:{}: `wall_hours` was REPLACED by `wall_time`, and the unit is now SECONDS.{}\n\n\
                 \x20   abort_if: \"wall_hours >= 8\"  ->  abort_if: \"work_time >= 28800\"\n\
                 \x20   limits: {{ wall_hours: 8 }}    ->  limits: {{ wall_time: 28800 }}\n\n\
                 \x20 There is no alias on purpose: renaming the key without converting the number turns \
                 an 8-hour ceiling into an 8-second one.\n\
                 \x20 `wall_time` is END-TO-END (a deadline; human waiting counts against it). \
                 `work_time` excludes time spent waiting for a human, and is usually what you want.",
                path.display(),
                n + 1,
                hint,
            );
        }
        Ok(())
    }

    /// Warn when a clock bound looks like it was written in hours.
    ///
    /// A fresh config has no old key to catch, so this is the only guard against the trap the rename
    /// creates. `work_time >= 8` is legal — a smoke test may want eight seconds — and is nearly
    /// always somebody thinking in hours. A warning, never an error: agg does not overrule a bound
    /// its operator actually meant.
    fn warn_suspicious_clock_bounds(&self) {
        const FLOOR: f64 = 60.0;
        const TERMS: [&str; 3] = ["wall_time", "human_wait_time", "work_time"];
        let mut sus: Vec<String> = Vec::new();

        for (what, v) in [
            ("limits.wall_time", self.sequence.limits.wall_time),
            ("limits.work_time", self.sequence.limits.work_time),
        ] {
            if let Some(v) = v {
                if v > 0.0 && v < FLOOR {
                    sus.push(format!("{what}: {v}"));
                }
            }
        }

        let mut exprs: Vec<(&str, &str)> = vec![("done_if", self.sequence.done_if.as_str())];
        for (what, e) in [("abort_if", &self.sequence.abort_if), ("notify_if", &self.sequence.notify_if)] {
            if let Some(e) = e.as_deref() {
                exprs.push((what, e));
            }
        }
        for (what, expr) in exprs {
            for term in TERMS {
                for rest in expr.split(term).skip(1) {
                    let n = rest
                        .trim_start_matches(|c: char| matches!(c, '>' | '<' | '=' | '!') || c.is_whitespace())
                        .split_whitespace()
                        .next()
                        .and_then(|t| t.trim_end_matches(')').parse::<f64>().ok());
                    if let Some(n) = n {
                        if n > 0.0 && n < FLOOR {
                            sus.push(format!("{what}: `{term}` compared against {n}"));
                        }
                    }
                }
            }
        }

        for s in sus {
            eprintln!(
                "  ⚠ {s} — the clock terms are in SECONDS. If you meant hours, multiply by 3600 (8h = 28800). Proceeding as written."
            );
        }
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
