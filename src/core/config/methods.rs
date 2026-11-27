//! `impl RelentlessConfig` behaviour: the ruler/worker backend resolution, the per-step merge into a
//! [`ResolvedStep`], the agent-name collection, `load` + its CI-friendly env overrides, plus the
//! partial-parse agent reader and the `env_u64`/`env_f64` parse helpers. Pure behaviour — the serde
//! shape and the `default_*` fns live in the parent module.

use super::*;
use anyhow::anyhow;
use std::path::Path;

impl RelentlessConfig {
    /// The **RULER**: the backend that runs the rubric verifiers and the summarizer. Reads `verifier.agent`.
    pub fn ruler_backend(&self) -> Result<&'static dyn AgentBackend> {
        for_name(&self.verifier.agent)
    }

    /// The default WORKER backend (`defaults.agent`) — for the banner/dashboard and for the paths
    /// that want "the primary agent" without a specific step. Per-step, use [`Self::resolve_step`].
    pub fn worker_backend(&self) -> Result<&'static dyn AgentBackend> {
        for_name(&self.defaults.agent)
    }

    /// The verifier model (`verifier.model`), resolved against the ruler. Absent = the ruler's cheap default.
    pub fn verifier_model(&self, ruler: &dyn AgentBackend) -> String {
        self.verifier.model.clone().unwrap_or_else(|| ruler.default_summary_model().to_string())
    }

    /// Merge a named step's body over [`Defaults`] into a [`ResolvedStep`]. Errors (listing the
    /// palette) if the name is not a key in `steps:` — a startup hard error, never a runtime surprise.
    pub fn resolve_step(&self, name: &str) -> Result<ResolvedStep> {
        let body = self.steps.get(name).ok_or_else(|| {
            let names: Vec<&str> = self.steps.keys().map(String::as_str).collect();
            anyhow!("unknown step `{name}` — not a key in `steps:`. defined: {}", names.join(", "))
        })?;
        let role = if body.role_prompt.is_some() || body.role_prompt_file.is_some() {
            (&body.role_prompt, &body.role_prompt_file)
        } else {
            (&self.defaults.role_prompt, &self.defaults.role_prompt_file)
        };
        Ok(ResolvedStep {
            name: name.to_string(),
            agent: body.agent.clone().unwrap_or_else(|| self.defaults.agent.clone()),
            model: body.model.clone().or_else(|| self.defaults.model.clone()),
            effort: body.effort.clone().or_else(|| self.defaults.effort.clone()),
            worker_args: body.worker_args.clone().unwrap_or_else(|| self.defaults.worker_args.clone()),
            state: body.state.clone().unwrap_or_else(|| self.defaults.state.clone()),
            retry: crate::retry::resolved(body.retry.as_ref().or(self.defaults.retry.as_ref())),
            timeout_secs: body.timeout.or(self.defaults.timeout).map(|d| d.0),
            memory: true,
            resume: None,
            news: None,
            user_settings: true,
            system_prompt: None,
            // the pair inherits AS A PAIR: a step naming either form replaces both of the defaults'.
            role_prompt: role.0.clone(),
            role_prompt_file: role.1.clone(),
            prompt: body.prompt.clone(),
            prompt_file: body.prompt_file.clone(),
            skip_verifiers: body.skip_verifiers,
            // Copy type → no clone. step overrides defaults; both absent ⇒ Isolation::None.
            isolation: body.isolation.or(self.defaults.isolation).unwrap_or_default(),
            image: body
                .image
                .clone()
                .or_else(|| self.defaults.image.clone())
                .unwrap_or_else(|| crate::isolation::DEFAULT_IMAGE.to_string()),
            // Normalised HERE, on the way in, exactly as the driver's builder does it — `writable`
            // subtracts by string, so `writable: [rlt/verifiers]` against `readonly: [rlt/verifiers/]`
            // would otherwise subtract nothing while reading like it worked. There is no
            // `defaults.readonly`: the YAML path has one defaults block and repeating the list per
            // step is the shape `sample_workflow.yaml` documents (the Rust path's templates are the
            // answer to that repetition).
            readonly: crate::isolation::normalize_paths(&body.readonly),
            writable: crate::isolation::normalize_paths(&body.writable),
            // UNION, never override: a step can add protected paths, never drop the default ones.
            write_deny: crate::protect::union(&self.defaults.write_deny, &body.write_deny),
            // YAML cannot name a driver-supplied backend; `agent:` is a string there.
            custom: None,
        })
    }

    /// The RUN-LEVEL blast-radius tier — the STRONGEST tier the defaults or any step names. The
    /// tier verifiers run in (§2.5, "verifiers take the run tier") and the run-level teardown hook
    /// (`on_stop`) fires in. Over-approximates deliberately: a step that overrides the defaults'
    /// tier downward does not lower it — confining a verifier that maybe didn't need it is harmless;
    /// leaving it unconfined when a confined worker ran is the escape.
    ///
    /// `container` counts. It used not to — verifiers and hooks were host tooling while the container
    /// tier could not run an agent, and the residual (a container worker rewrites a verifier script
    /// that then runs on the host) was documented rather than closed. With an agent image in the
    /// container (the image contract), the image that runs the agent runs the verifiers too
    /// ([`Self::verifier_image`]), and the residual is closed. Hooks stay host tooling
    /// ([`crate::hooks::run`]).
    pub fn run_isolation(&self) -> crate::isolation::Isolation {
        let base = self.defaults.isolation.unwrap_or_default();
        self.steps.values().filter_map(|b| b.isolation).fold(base, |t, s| t.strongest(s))
    }

    /// The image verifiers run in when [`Self::run_isolation`] is `container`: `verifier.image`, else
    /// `defaults.image`, else the ONE image every container step resolves to, else the default
    /// base image. `Err` (the problem, for `verifier.image`) when container steps name several
    /// different images and neither key says which one the verifiers' toolchain is in.
    pub fn verifier_image(&self) -> Result<String, String> {
        if let Some(i) = self.verifier.image.as_ref().or(self.defaults.image.as_ref()) {
            return Ok(i.clone());
        }
        let mut images: Vec<String> = self
            .steps
            .keys()
            .filter_map(|n| self.resolve_step(n).ok())
            .filter(|s| s.isolation == crate::isolation::Isolation::Container)
            .map(|s| s.image)
            .collect();
        images.sort();
        images.dedup();
        match images.as_slice() {
            [] => Ok(crate::isolation::DEFAULT_IMAGE.to_string()),
            [one] => Ok(one.clone()),
            many => Err(format!(
                "the container steps run in {} different images ({}) and neither `verifier.image` nor \
                 `defaults.image` says which one the verifiers run in — set `verifier.image` to the image \
                 with your verifiers' toolchain",
                many.len(),
                many.join(", ")
            )),
        }
    }

    /// Every distinct agent named anywhere (defaults + verifier + each step) — so `rlt doctor` and the
    /// capability check can cover EVERY agent the sequence names (§7.3), not just one.
    pub fn agent_names(&self) -> Vec<String> {
        let mut names = vec![self.defaults.agent.clone(), self.verifier.agent.clone()];
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
    /// config — for `doctor` / `skills install`, which must name an agent when rlt.yaml may be
    /// absent or broken. Missing → `"claude"`.
    pub fn agent_name(path: &Path) -> String {
        read_agent(path, false)
    }

    /// Read ONLY the ruler agent (`verifier.agent`, else `defaults.agent`) — for `plan` / `verifier`,
    /// which run verifiers on the RULER off rlt.yaml alone.
    pub fn ruler_name(path: &Path) -> String {
        read_agent(path, true)
    }

    pub fn load(path: &Path) -> Result<Self> {
        // BEFORE serde, because `deny_unknown_fields` would report `wall_hours` as merely unknown
        // and the reader would "fix" it by renaming — which is the one migration that silently
        // breaks. Checked on the RAW text so it catches the key AND the condition term in one pass.
        Self::reject_removed_keys(path)?;
        let mut cfg: RelentlessConfig = crate::util::load_yaml(path)?;
        cfg.finish_load();
        Ok(cfg)
    }

    /// What [`Self::load`] does after serde: the env overrides, then the clock-unit warning. Shared
    /// with `rlt check`, which parses the text itself so it can collect every problem.
    pub(crate) fn finish_load(&mut self) {
        self.apply_env_overrides();
        self.warn_suspicious_clock_bounds();
    }

    /// A hard error naming the replacement AND doing the arithmetic.
    ///
    /// `wall_hours` became `wall_time`, in **seconds** — a 3600x unit change. serde's
    /// `deny_unknown_fields` catches `limits.wall_hours`, but its message ("unknown field") invites
    /// exactly the wrong repair, and it cannot see `abort_if: "wall_hours >= 8"` at all because that
    /// is a string. Both surfaces are caught here, in the raw YAML, and the message converts the
    /// value for the reader. See `internal/HUMAN_LOOP.md` §7.4.1.
    pub(crate) fn reject_removed_keys(path: &Path) -> Result<()> {
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
    /// always somebody thinking in hours. A warning, never an error: rlt does not overrule a bound
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

    /// CI-friendly env overrides, re-homed under the new shape (§4.1). `RLT_MODEL` → defaults.model,
    /// `RLT_COST_TOTAL` → sequence.limits.cost, `RLT_TOKEN_BUDGET` → sequence.limits.tokens.
    pub(super) fn apply_env_overrides(&mut self) {
        if let Ok(v) = std::env::var("RLT_MODEL") {
            self.defaults.model = Some(v);
        }
        if let Some(v) = env_u64("RLT_HEARTBEAT_SECS") {
            self.heartbeat_secs = v;
        }
        if let Some(v) = env_u64("RLT_WATCHDOG_IDLE_SECS") {
            self.watchdog.idle_secs = v;
        }
        if let Some(v) = env_u64("RLT_WATCHDOG_CPU_GRACE") {
            self.watchdog.cpu_grace = v;
        }
        if let Some(v) = env_u64("RLT_RATELIMIT_BACKOFF") {
            self.ratelimit_backoff_secs = v;
        }
        if let Some(v) = env_f64("RLT_COST_TOTAL") {
            self.sequence.limits.cost = Some(v);
        }
        if let Some(v) = env_u64("RLT_TOKEN_BUDGET") {
            self.sequence.limits.tokens = Some(v);
        }
        if let Some(v) = env_u64("RLT_MEMORY_MAX_KB") {
            self.memory.max_kb = if v == 0 { None } else { Some(v) };
        }
        if let Some(v) = env_u64("RLT_MEMORY_INJECT_KB") {
            self.memory.inject_kb = if v == 0 { None } else { Some(v) };
        }
    }
}

/// Partial-parse the agent name(s) from rlt.yaml without the deny_unknown_fields full parse, so it
/// survives a config that is missing / broken / half-written. `ruler` picks `verifier.agent`.
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
        verifier: AgentOnly,
    }
    let parsed: Option<Partial> =
        std::fs::read_to_string(path).ok().and_then(|t| serde_yaml::from_str(&t).ok());
    let p = parsed.unwrap_or_default();
    if ruler {
        p.verifier.agent.or(p.defaults.agent)
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
