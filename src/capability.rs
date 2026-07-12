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
//! `over_cost` is `cost_spent >= cost_limit`. An agent that never reports cost leaves `cost_spent`
//! at 0.0 forever, so the predicate is never true, so the guard **never fires** — and an
//! autonomous loop runs unbounded, spending real money, with no error anywhere. Of the three
//! agents surveyed (Claude, Codex, Copilot) **only Claude reports dollars at all**: Codex reports
//! none, Copilot reports a credit multiplier that is not USD.
//!
//! The same trap applies to `over_budget` (needs output tokens), `resume_sessions` (needs a
//! resume handle), LLM judges and the summarizer (need a genuinely tools-off one-shot call — which
//! neither Codex nor Copilot has a verified way to do), and `effort:`.
//!
//! So: every such demand is checked ONCE, at startup, against the backend's declared
//! [`Capabilities`], and a mismatch is a hard error naming the config key, the agent, and the fix.
//! **A capability the agent lacks must never degrade into a quiet no-op.**

use crate::backend::AgentBackend;
use crate::core::config::AggConfig;
use crate::core::config::GoalsConfig;
use crate::core::model::JudgeSpec;
use anyhow::Result;

/// One thing the config asks of the agent, and the capability it needs.
struct Demand {
    /// true when the config actually asks for this.
    wanted: bool,
    /// true when the backend can deliver it.
    provided: bool,
    /// the agg.yaml / goals.yaml key that asked for it.
    key: &'static str,
    /// what breaks, in the user's terms, if we let this through.
    consequence: &'static str,
    /// how to resolve it.
    fix: String,
}

/// Check the config against the active backend. Called once, before the loop starts.
///
/// Returns an error listing EVERY unmet demand (not just the first), because a user switching
/// agents wants the whole list in one go, not a game of whack-a-mole.
pub fn check(cfg: &AggConfig, goals: &GoalsConfig, backend: &dyn AgentBackend) -> Result<()> {
    let caps = backend.capabilities();
    let agent = backend.name();

    // an LLM judge or the summarizer both need a non-agentic, tools-off call.
    let uses_llm_judge = goals.goals.iter().any(|g| matches!(g.judge, JudgeSpec::Llm { .. }));
    let uses_summarizer = cfg.summary.enabled;

    // A guard can be asked for two ways: a ceiling in agg.yaml, or the predicate named in a
    // stop/halt condition in goals.yaml. Either one means the user expects it to fire.
    let named = |predicate: &str| {
        goals.stop_when.contains(predicate)
            || goals.halt_when.as_deref().unwrap_or("").contains(predicate)
    };

    // Refusing a cost guard is right, but on its own it leaves the operator with NO spend
    // protection — the very outcome the refusal exists to prevent. If the agent can cap itself
    // some other way (Copilot: `--max-ai-credits`), say so.
    let cost_fix = match backend.spend_ceiling_hint() {
        Some(hint) => format!(
            "remove the cost guard, or use an agent that reports a dollar cost.\n           \
             DO NOT leave an autonomous loop with no spend ceiling at all — `{agent}` can cap \
             itself instead: {hint}"
        ),
        None => "remove the cost guard (an unbounded autonomous loop is a real risk), \
                 or use an agent that reports a dollar cost"
            .to_string(),
    };

    let demands = [
        Demand {
            wanted: cfg.budget.total.is_some() || named("over_budget"),
            provided: caps.reports_output_tokens,
            key: "budget.total / halt_when: over_budget",
            consequence: "the token guard would NEVER fire — the loop would run unbounded",
            fix: "remove the budget guard, or use an agent that reports token usage".to_string(),
        },
        Demand {
            wanted: cfg.cost.total.is_some() || named("over_cost"),
            provided: caps.reports_cost_usd,
            key: "cost.total / halt_when: over_cost",
            consequence: "the SPEND guard would NEVER fire — the loop would run unbounded, \
                          spending real money",
            fix: cost_fix,
        },
        Demand {
            wanted: cfg.resume_sessions,
            provided: caps.supports_resume,
            key: "resume_sessions: true",
            consequence: "every session would silently start with a FRESH context instead of \
                          continuing the last one",
            fix: "set `resume_sessions: false`, or use an agent that supports resuming a session"
                .to_string(),
        },
        Demand {
            wanted: !cfg.effort.is_empty(),
            provided: caps.supports_effort,
            key: "effort",
            consequence: "the effort level would be silently ignored",
            fix: "set `effort: \"\"`, or use an agent that accepts a thinking-effort level".to_string(),
        },
        Demand {
            wanted: uses_llm_judge,
            provided: caps.supports_one_shot,
            key: "a goal with `judge: { kind: llm }`",
            consequence: "the judge could not be run with tools disabled — an LLM judge that can \
                          run tools is not a judge, it can edit the very thing it is grading",
            fix: "use script judges, or use an agent that can make a tools-off one-shot call".to_string(),
        },
        Demand {
            wanted: uses_summarizer,
            provided: caps.supports_one_shot,
            key: "summary.enabled: true",
            consequence: "the summarizer has no way to make a plain, non-agentic model call",
            fix: "set `summary: { enabled: false }`, or use an agent that supports a one-shot call"
                .to_string(),
        },
    ];

    let unmet: Vec<&Demand> = demands.iter().filter(|d| d.wanted && !d.provided).collect();
    if unmet.is_empty() {
        return Ok(());
    }

    let mut msg = format!(
        "the `{agent}` agent cannot do what this config asks of it ({} problem(s)).\n\
         These are refused at startup rather than silently ignored — a guard that never fires is \
         worse than no guard.\n",
        unmet.len()
    );
    for d in unmet {
        msg.push_str(&format!(
            "\n  ✗ {}\n      would mean: {}\n      fix: {}\n",
            d.key, d.consequence, d.fix
        ));
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

    /// An LLM judge needs a TOOLS-OFF call. Neither Codex nor Copilot has a verified way to do
    /// that, and a judge that can run tools can edit what it is grading.
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

    /// Claude can do everything, so a full-featured config must pass cleanly.
    #[test]
    fn claude_satisfies_a_fully_loaded_config() {
        let cfg = cfg_yaml(
            "project: p\nmodel: m\nresume_prompt: R\nsummary: { enabled: true }\n\
             effort: max\ncost: { total: 5.0 }\nbudget: { total: 10 }\nresume_sessions: true\n",
        );
        check(&cfg, &goals_yaml(SCRIPT_GOALS), crate::backend::active()).expect("claude does it all");
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
