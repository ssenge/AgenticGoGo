//! Refuse to start a run whose config demands something the chosen agent cannot do.
//!
//! # Why this module exists
//! Agents are not interchangeable, and the gaps are silent by default. The worst case:
//!
//! ```text
//!   halt_when: over_cost        # "stop when I've spent $5"
//!   cost: { total: 5.0 }
//!   agent: <one that does not report dollar cost>
//! ```
//!
//! `over_cost` is `cost_spent > cost_limit` (see `core::stop`). An agent that never reports cost leaves `cost_spent`
//! at 0.0 forever, so the predicate is never true, so the guard **never fires** — and an
//! autonomous loop runs unbounded, spending real money, with no error anywhere. Of the three
//! agents surveyed (Claude, Codex, Copilot) **only Claude reports dollars at all**: Codex reports
//! none, Copilot reports a credit multiplier that is not USD.
//!
//! The same trap applies to `over_budget` (needs output tokens), `resume_sessions` (needs a resume
//! handle), LLM judges and the summarizer (need a read-only one-shot call — which all three agents
//! can do, each by its own mechanism), and `effort:`.
//!
//! So: every such demand is checked ONCE, at startup, against the backend's declared
//! [`Capabilities`], and a mismatch is a hard error naming the config key, the agent, and the fix.
//! **A capability the agent lacks must never degrade into a quiet no-op.**

use crate::core::config::AggConfig;
use crate::core::model::{Judge, JudgeKind};
use anyhow::Result;

/// Check the config + resolved run-set against EVERY agent the sequence names (§7.3). The
/// one-shot demands (LLM judges + summarizer) fall on the RULER; the token/cost/effort demands fall
/// on each STEP's worker backend. Returns an error listing EVERY problem at once.
///
/// A capability the agent lacks must never degrade into a quiet no-op: a guard that never fires is
/// worse than no guard.
pub fn check(cfg: &AggConfig, judges: &[Judge]) -> Result<()> {
    let mut problems: Vec<String> = Vec::new();

    // ── one-shot demands land on the RULER (§7.3) ──
    let ruler = cfg.ruler_backend()?;
    let uses_llm_judge = judges.iter().any(|j| matches!(j.kind, JudgeKind::Llm { .. }));
    let uses_summarizer = cfg.summary.enabled;
    if (uses_llm_judge || uses_summarizer) && !ruler.capabilities().supports_one_shot {
        problems.push(format!(
            "the ruler `{}` cannot make a tools-off one-shot call, which {} need.\n      \
             fix: use script judges + `summary.enabled: false`, or a ruler that supports it",
            ruler.name(),
            if uses_llm_judge { "LLM judges / the summarizer" } else { "the summarizer" },
        ));
    }

    // ── token / cost / effort demands land on each WORKER agent the sequence names ──
    let named = |p: &str| {
        cfg.sequence.done_if.contains(p) || cfg.sequence.abort_if.as_deref().unwrap_or("").contains(p)
    };
    let budget_wanted = cfg.sequence.budget.total.is_some() || named("over_budget");
    let cost_wanted = cfg.sequence.cost.total.is_some() || named("over_cost");

    for step_name in cfg.steps.keys() {
        let step = cfg.resolve_step(step_name)?;
        let b = step.backend()?;
        let caps = b.capabilities();
        let agent = b.name();
        let model = step.model(b).to_string();
        let effort = step.effort(b).to_string();

        if budget_wanted && !caps.reports_output_tokens {
            problems.push(format!(
                "step `{step_name}` (agent `{agent}`): `budget.total`/`over_budget` set, but it \
                 does not report token usage — the token guard would NEVER fire."
            ));
        }
        if cost_wanted && !caps.reports_cost_usd {
            let hint = match b.spend_ceiling_hint() {
                Some(h) => format!("`{agent}` can cap itself instead: {h}"),
                None => "remove the cost guard, or use an agent that reports a dollar cost".to_string(),
            };
            problems.push(format!(
                "step `{step_name}` (agent `{agent}`): `cost.total`/`over_cost` set, but it cannot \
                 report dollars — the SPEND guard would NEVER fire, spending real money. {hint}"
            ));
        }
        if !effort.is_empty() && !caps.supports_effort {
            problems.push(format!(
                "step `{step_name}` (agent `{agent}`): `effort: {effort}` set, but the agent does \
                 not accept a thinking-effort level."
            ));
        }
        if let Some(c) = b.config_conflict(&model, &effort) {
            problems.push(format!("step `{step_name}` (agent `{agent}`): model + effort — {c}"));
        }
    }

    if problems.is_empty() {
        return Ok(());
    }
    let mut msg = format!(
        "the config asks for {} thing(s) the chosen agent(s) cannot do.\n\
         These are refused at startup rather than silently ignored — a guard that never fires is \
         worse than no guard.\n",
        problems.len()
    );
    for p in problems {
        msg.push_str(&format!("\n  ✗ {p}\n"));
    }
    anyhow::bail!(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Capabilities, OneShot, SessionReport, SessionSpec};
    use std::path::Path;
    use std::process::Command;

    /// A deliberately minimal agent: it can run a worker and nothing else. This is not a straw man
    /// — it is roughly the floor that the Copilot survey describes (no dollar cost, no verified
    /// tools-off one-shot).
    struct Barebones;
    impl crate::backend::AgentBackend for Barebones {
        fn name(&self) -> &'static str { "barebones" }
        fn bin(&self) -> &'static str { "barebones" }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                reports_output_tokens: false,
                reports_cost_usd: false,
                supports_resume: false,
                supports_effort: false,
                detects_rate_limits: false,
                supports_one_shot: false,
            }
        }
        fn default_model(&self) -> &'static str { "m" }
        fn default_summary_model(&self) -> &'static str { "m" }
        fn session_command(&self, _s: &SessionSpec) -> Command { Command::new("true") }
        fn parse_event(&self, _l: &str) -> Option<crate::backend::stream::Event> { None }
        fn parse_usage(&self, _l: &str) -> Option<u64> { None }
        fn parse_session_id(&self, _l: &str) -> Option<String> { None }
        fn parse_result(&self, _l: &str) -> Option<SessionReport> { None }
        fn one_shot(&self, _p: &str, _m: &str, _t: u64, _c: Option<&Path>) -> Result<OneShot, String> {
            unreachable!("gated by supports_one_shot")
        }
        fn is_installed(&self) -> bool { true }
        fn preflight(&self) -> Result<()> { Ok(()) }
    }

    /// Like Barebones, but it CAN cap its own spend — the shape Copilot actually has
    /// (`--max-ai-credits`: it bills in credits, so it cannot report dollars, but it is not
    /// therefore unboundable).
    struct SelfCapping;
    impl crate::backend::AgentBackend for SelfCapping {
        fn name(&self) -> &'static str { "selfcapping" }
        fn bin(&self) -> &'static str { "selfcapping" }
        fn capabilities(&self) -> Capabilities { Barebones.capabilities() }
        fn spend_ceiling_hint(&self) -> Option<&'static str> {
            Some("pass `--max-ai-credits <n>` via agg.yaml `worker_args`")
        }
        fn default_model(&self) -> &'static str { "m" }
        fn default_summary_model(&self) -> &'static str { "m" }
        fn session_command(&self, _s: &SessionSpec) -> Command { Command::new("true") }
        fn parse_event(&self, _l: &str) -> Option<crate::backend::stream::Event> { None }
        fn parse_usage(&self, _l: &str) -> Option<u64> { None }
        fn parse_session_id(&self, _l: &str) -> Option<String> { None }
        fn parse_result(&self, _l: &str) -> Option<SessionReport> { None }
        fn one_shot(&self, _p: &str, _m: &str, _t: u64, _c: Option<&Path>) -> Result<OneShot, String> {
            unreachable!("gated by supports_one_shot")
        }
        fn is_installed(&self) -> bool { true }
        fn preflight(&self) -> Result<()> { Ok(()) }
    }

    /// Refusing the cost guard is right, but it must not leave the operator with NO ceiling at
    /// all. When the agent can cap itself, the refusal has to SAY SO — otherwise the safest
    /// reading of the error ("just delete the cost guard") is the most dangerous action.
    #[test]
    fn a_refused_cost_guard_points_at_the_agents_own_ceiling_when_it_has_one() {
        let cfg = cfg_yaml(
            "project: p\nmodel: m\nresume_prompt: R\nsummary: { enabled: false }\n\
             effort: \"\"\ncost: { total: 5.0 }\n",
        );
        let err = check(&cfg, &goals_yaml(SCRIPT_GOALS), &SelfCapping).unwrap_err().to_string();
        assert!(err.contains("--max-ai-credits"), "must name the agent's own ceiling:\n{err}");
        assert!(
            err.contains("DO NOT leave an autonomous loop with no spend ceiling"),
            "must warn against the naive fix:\n{err}"
        );
    }

    fn goals_yaml(body: &str) -> GoalsConfig {
        serde_yaml::from_str(body).expect("test goals parse")
    }
    fn cfg_yaml(body: &str) -> AggConfig {
        serde_yaml::from_str(body).expect("test config parse")
    }

    const SCRIPT_GOALS: &str =
        "goals:\n  - id: g\n    type: binary\n    judge: { kind: script, cmd: \"true\" }\nstop_when: g\n";

    /// THE bug this module exists to prevent: a spend guard against an agent that cannot report
    /// spend must be a startup ERROR, not a guard that quietly never fires.
    #[test]
    fn a_cost_guard_on_an_agent_that_cannot_report_cost_is_refused() {
        let cfg = cfg_yaml(
            "project: p\nmodel: m\nresume_prompt: R\nsummary: { enabled: false }\n\
             effort: \"\"\ncost: { total: 5.0 }\n",
        );
        let err = check(&cfg, &goals_yaml(SCRIPT_GOALS), &Barebones).unwrap_err().to_string();
        assert!(err.contains("cost.total"), "must name the offending key:\n{err}");
        assert!(err.contains("spending real money"), "must say what breaks:\n{err}");
        assert!(err.contains("barebones"), "must name the agent:\n{err}");
    }

    #[test]
    fn a_token_budget_on_an_agent_that_cannot_report_tokens_is_refused() {
        let cfg = cfg_yaml(
            "project: p\nmodel: m\nresume_prompt: R\nsummary: { enabled: false }\n\
             effort: \"\"\nbudget: { total: 1000 }\n",
        );
        let err = check(&cfg, &goals_yaml(SCRIPT_GOALS), &Barebones).unwrap_err().to_string();
        assert!(err.contains("budget.total"), "got:\n{err}");
        assert!(err.contains("run unbounded"), "got:\n{err}");
    }

    /// An LLM judge needs a READ-ONLY call — a judge that can WRITE can edit what it is grading.
    /// All three real agents can do this (each by its own mechanism); `Barebones` cannot, which is
    /// what this test pins: the refusal must fire for ANY backend that declares it cannot.
    #[test]
    fn an_llm_judge_on_an_agent_without_a_one_shot_call_is_refused() {
        let cfg = cfg_yaml(
            "project: p\nmodel: m\nresume_prompt: R\nsummary: { enabled: false }\neffort: \"\"\n",
        );
        let goals = goals_yaml(
            "goals:\n  - id: g\n    type: binary\n    \
             judge: { kind: llm, model: m, rubric: r.md, inputs: [] }\nstop_when: g\n",
        );
        let err = check(&cfg, &goals, &Barebones).unwrap_err().to_string();
        assert!(err.contains("kind: llm"), "got:\n{err}");
        assert!(err.contains("edit the very thing it is grading"), "got:\n{err}");
    }

    /// Every unmet demand at once — a user switching agents gets the whole list, not whack-a-mole.
    #[test]
    fn all_unmet_demands_are_reported_together() {
        let cfg = cfg_yaml(
            "project: p\nmodel: m\nresume_prompt: R\nsummary: { enabled: true }\n\
             effort: max\ncost: { total: 5.0 }\nbudget: { total: 10 }\nresume_sessions: true\n",
        );
        let err = check(&cfg, &goals_yaml(SCRIPT_GOALS), &Barebones).unwrap_err().to_string();
        for key in ["cost.total", "budget.total", "resume_sessions", "effort", "summary.enabled"] {
            assert!(err.contains(key), "every unmet demand must be listed; missing {key}:\n{err}");
        }
    }

    /// REGRESSION: a pair of keys that are each individually legal but MUTUALLY exclusive must be
    /// refused. Copilot declares `supports_effort: true` (honestly — the flag exists) and defaults
    /// to `model: auto`, so every per-feature check passes; but Copilot rejects the two together
    /// and EVERY worker session dies in seconds with 0 tokens, while the loop halts on
    /// over_iterations having done nothing. `agg doctor` used to print
    /// "✔ agent `copilot` can do everything this config asks" for precisely that config.
    #[test]
    fn a_pair_of_individually_legal_keys_the_agent_cannot_combine_is_refused() {
        let cop = crate::backend::for_name("copilot").unwrap();
        let bad = cfg_yaml(
            "project: p\nagent: copilot\nmodel: auto\neffort: high\nresume_prompt: R\n\
             summary: { enabled: false }\n",
        );
        let err = check(&bad, &goals_yaml(SCRIPT_GOALS), cop).unwrap_err().to_string();
        assert!(err.contains("model: auto"), "must name the offending pair:\n{err}");
        assert!(err.contains("0 tokens"), "must say what actually breaks:\n{err}");

        // …and each key ALONE is still perfectly fine — this must not become a blanket ban.
        let auto_no_effort = cfg_yaml(
            "project: p\nagent: copilot\nmodel: auto\neffort: \"\"\nresume_prompt: R\n\
             summary: { enabled: false }\n",
        );
        check(&auto_no_effort, &goals_yaml(SCRIPT_GOALS), cop).expect("`auto` with no effort is the default, and works");

        let named_model_with_effort = cfg_yaml(
            "project: p\nagent: copilot\nmodel: claude-sonnet-4.5\neffort: high\nresume_prompt: R\n\
             summary: { enabled: false }\n",
        );
        check(&named_model_with_effort, &goals_yaml(SCRIPT_GOALS), cop)
            .expect("a concrete model may carry an effort");
    }

    /// Claude can do everything, so a full-featured config must pass cleanly.
    #[test]
    fn claude_satisfies_a_fully_loaded_config() {
        let cfg = cfg_yaml(
            "project: p\nmodel: m\nresume_prompt: R\nsummary: { enabled: true }\n\
             effort: max\ncost: { total: 5.0 }\nbudget: { total: 10 }\nresume_sessions: true\n",
        );
        let claude = crate::backend::for_name("claude").unwrap();
        check(&cfg, &goals_yaml(SCRIPT_GOALS), claude).expect("claude does it all");
    }

    /// A config that asks for nothing special runs on ANY agent, however limited.
    #[test]
    fn a_modest_config_runs_on_the_barebones_agent() {
        let cfg = cfg_yaml(
            "project: p\nmodel: m\nresume_prompt: R\nsummary: { enabled: false }\neffort: \"\"\n",
        );
        check(&cfg, &goals_yaml(SCRIPT_GOALS), &Barebones).expect("nothing demanded, nothing refused");
    }
}
