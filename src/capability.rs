//! Refuse to start a run whose config demands something the chosen agent cannot do.
//!
//! # Why this module exists
//! Agents are not interchangeable, and the gaps are silent by default. The worst case:
//!
//! ```text
//!   abort_if: over_cost              # "stop when I've spent $5"
//!   limits: { cost: 5.0 }
//!   agent: <one that does not report dollar cost>
//! ```
//!
//! `over_cost` is `cost_spent > cost_limit` (see `core::stop`). An agent that never reports cost leaves `cost_spent`
//! at 0.0 forever, so the predicate is never true, so the guard **never fires** — and an
//! autonomous loop runs unbounded, spending real money, with no error anywhere. Of the three
//! agents surveyed (Claude, Codex, Copilot) **only Claude reports dollars at all**: Codex reports
//! none, Copilot reports a credit multiplier that is not USD.
//!
//! The same trap applies to `over_budget` (needs output tokens), LLM judges and the summarizer
//! (need a read-only one-shot call — which all three agents can do, each by its own mechanism),
//! and `effort:`. (`resume_sessions` is gone entirely — a per-agent session id can't cross a mixed
//! sequence, so `deny_unknown_fields` refuses the key at PARSE time now, §7.3.)
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
    // Non-fatal but LOUD (§7.3): a guard that is inert on a given agent, yet the loop still has a
    // working ceiling (the agent caps itself). Printed whether or not we go on to bail.
    let mut warnings: Vec<String> = Vec::new();

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
    let budget_wanted = cfg.sequence.limits.tokens.is_some() || named("over_budget");
    let cost_wanted = cfg.sequence.limits.cost.is_some() || named("over_cost");

    for step_name in cfg.steps.keys() {
        let step = cfg.resolve_step(step_name)?;
        let b = step.backend()?;
        let caps = b.capabilities();
        let agent = b.name();
        let model = step.model(b).to_string();
        let effort = step.effort(b).to_string();

        if budget_wanted && !caps.reports_output_tokens {
            problems.push(format!(
                "step `{step_name}` (agent `{agent}`): `limits.tokens`/`over_budget` set, but it \
                 does not report token usage — the token guard would NEVER fire."
            ));
        }
        if cost_wanted && !caps.reports_cost_usd {
            // §7.3: only Claude reports dollars, and the owner's ruling is acceptable-but-LOUD. If
            // the agent can cap ITSELF, the loop is not left unbounded — the dollar guard is merely
            // inert on this agent, so WARN, don't refuse. Refuse ONLY when there is no working
            // ceiling at all (no dollar reporting AND no self-cap): an autonomous loop with no bound
            // on real money. `spend_ceiling_hint()` is exactly the "does it have a ceiling?" oracle.
            match b.spend_ceiling_hint() {
                Some(h) => warnings.push(format!(
                    "limits.cost/over_cost is INERT on `{agent}` (step `{step_name}`) — it cannot \
                     report dollars, so this guard will never fire. Cap it directly instead: {h}"
                )),
                None => problems.push(format!(
                    "step `{step_name}` (agent `{agent}`): `limits.cost`/`over_cost` set, but it \
                     cannot report dollars and has no self-cap — the SPEND guard would NEVER fire, \
                     leaving an autonomous loop spending real money with no ceiling at all. Remove \
                     the cost guard, or use an agent that reports a dollar cost."
                )),
            }
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

    // Loud but non-fatal (§7.3): an inert-but-still-bounded guard. Surface it whether or not we
    // then bail on a hard problem — a run that proceeds should still know its cost guard is inert.
    for w in &warnings {
        eprintln!("⚠ {w}");
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
    use crate::core::model::{Judge, JudgeKind, Lifecycle};

    fn cfg_yaml(body: &str) -> AggConfig {
        serde_yaml::from_str(body).expect("test config parse")
    }

    /// A resolved script judge in the DoD-set — the common case.
    fn script_judge(name: &str) -> Judge {
        Judge {
            name: name.into(),
            kind: JudgeKind::Script { path: format!("{name}.sh").into() },
            invariant: false,
            in_dod: true,
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        }
    }

    /// A resolved LLM (`.md`) judge — the one that demands a tools-off one-shot call on the RULER.
    fn llm_judge(name: &str) -> Judge {
        Judge {
            name: name.into(),
            kind: JudgeKind::Llm { path: format!("{name}.md").into(), inputs: vec![] },
            invariant: false,
            in_dod: true,
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        }
    }

    // NOTE (C2 rewrite): the old fixtures injected a hand-rolled `Barebones`/`SelfCapping` backend
    // as the third arg to `check`. The per-step check (§7.3) resolves each agent from the CONFIG via
    // `for_name`, so a capability floor can only be expressed by a REAL agent now. The three shipped
    // agents all report tokens and support a one-shot call, so the "no token report" and "no
    // one-shot" refusal paths are defensive-only and no longer reachable through the config route.
    // codex (no dollars, no self-cap) and copilot (no dollars, self-caps via `--max-ai-credits`)
    // cover the cost policy — the branch §7.3 actually changed.

    /// THE bug this module exists to prevent: a spend guard on an agent that cannot report dollars
    /// AND cannot cap itself (codex) is a startup ERROR, not a guard that quietly never fires.
    #[test]
    fn a_cost_guard_on_an_agent_that_cannot_self_cap_is_refused() {
        let cfg = cfg_yaml(
            "project: p\ndefaults: { agent: codex }\njudge: { agent: claude }\n\
             summary: { enabled: false }\nsteps: { work: {} }\n\
             sequence: { steps: [work], limits: { cost: 5.0 } }\n",
        );
        let err = check(&cfg, &[script_judge("g")]).unwrap_err().to_string();
        assert!(err.contains("limits.cost"), "must name the offending key:\n{err}");
        assert!(err.contains("spending real money"), "must say what breaks:\n{err}");
        assert!(err.contains("codex"), "must name the agent:\n{err}");
    }

    /// §7.3's policy change: a cost guard on an agent that CANNOT report dollars but CAN cap itself
    /// (copilot, via `--max-ai-credits`) is acceptable-but-loud — a WARNING, not a refusal. The loop
    /// is not left unbounded, so it must be allowed to start.
    #[test]
    fn a_cost_guard_on_a_self_capping_agent_is_allowed() {
        let cfg = cfg_yaml(
            "project: p\ndefaults: { agent: copilot }\njudge: { agent: claude }\n\
             summary: { enabled: false }\nsteps: { work: {} }\n\
             sequence: { steps: [work], limits: { cost: 5.0 } }\n",
        );
        check(&cfg, &[script_judge("g")])
            .expect("copilot self-caps (--max-ai-credits), so an inert dollar guard only warns");
    }

    /// REGRESSION: a pair of keys each individually legal but MUTUALLY exclusive must be refused.
    /// Copilot declares `supports_effort: true` and defaults to `model: auto`, so every per-feature
    /// check passes; but Copilot rejects the two together and every worker session dies in seconds
    /// with 0 tokens, while the loop halts on over_iterations having done nothing.
    #[test]
    fn a_model_effort_pair_the_agent_cannot_combine_is_refused() {
        let bad = cfg_yaml(
            "project: p\ndefaults: { agent: copilot, model: auto, effort: high }\n\
             judge: { agent: claude }\nsummary: { enabled: false }\nsteps: { work: {} }\n\
             sequence: { steps: [work] }\n",
        );
        let err = check(&bad, &[script_judge("g")]).unwrap_err().to_string();
        assert!(err.contains("model: auto"), "must name the offending pair:\n{err}");
        assert!(err.contains("0 tokens"), "must say what actually breaks:\n{err}");

        // …and each key ALONE is still fine — this must not become a blanket ban.
        let auto_no_effort = cfg_yaml(
            "project: p\ndefaults: { agent: copilot, model: auto, effort: \"\" }\n\
             judge: { agent: claude }\nsummary: { enabled: false }\nsteps: { work: {} }\n\
             sequence: { steps: [work] }\n",
        );
        check(&auto_no_effort, &[script_judge("g")]).expect("`auto` with no effort is the default, and works");

        let named_model_with_effort = cfg_yaml(
            "project: p\ndefaults: { agent: copilot, model: claude-sonnet-4.5, effort: high }\n\
             judge: { agent: claude }\nsummary: { enabled: false }\nsteps: { work: {} }\n\
             sequence: { steps: [work] }\n",
        );
        check(&named_model_with_effort, &[script_judge("g")]).expect("a concrete model may carry an effort");
    }

    /// §7.3: the one-shot demand (an LLM judge / the summarizer) lands on the RULER, not the worker.
    /// The exact config the old code wrongly refused: a Codex worker with a Claude ruler and an
    /// `.md` judge. The one-shot host is the ruler (claude), so this must be ACCEPTED — the old
    /// check demanded it of the worker and refused a perfectly valid config.
    #[test]
    fn an_llm_judge_demand_lands_on_the_ruler_not_the_worker() {
        let cfg = cfg_yaml(
            "project: p\ndefaults: { agent: codex }\njudge: { agent: claude }\n\
             summary: { enabled: true }\nsteps: { work: {} }\nsequence: { steps: [work] }\n",
        );
        check(&cfg, &[llm_judge("rubric")]).expect("the ruler (claude) hosts the one-shot; the codex worker is irrelevant");
    }

    /// Claude can do everything, so a full-featured config must pass cleanly.
    #[test]
    fn claude_satisfies_a_fully_loaded_config() {
        let cfg = cfg_yaml(
            "project: p\ndefaults: { agent: claude, effort: max }\njudge: { agent: claude }\n\
             summary: { enabled: true }\nsteps: { work: {} }\n\
             sequence: { steps: [work], limits: { cost: 5.0, tokens: 10 } }\n",
        );
        check(&cfg, &[llm_judge("rubric"), script_judge("build")]).expect("claude does it all");
    }

    /// A config that asks for nothing special runs on ANY agent, however limited.
    #[test]
    fn a_modest_config_runs_on_any_agent() {
        let cfg = cfg_yaml(
            "project: p\ndefaults: { agent: codex }\njudge: { agent: claude }\n\
             summary: { enabled: false }\nsteps: { work: {} }\nsequence: { steps: [work] }\n",
        );
        check(&cfg, &[script_judge("g")]).expect("nothing demanded, nothing refused");
    }

    /// §7.3: the check is PER STEP. A mixed sequence — a claude step (reports dollars) and a codex
    /// step (does not, and cannot self-cap) — under a cost guard must be refused, and the refusal
    /// must name the CODEX step, not blanket-refuse or silently pass over it.
    #[test]
    fn the_check_is_per_step() {
        let cfg = cfg_yaml(
            "project: p\ndefaults: { agent: claude }\njudge: { agent: claude }\n\
             summary: { enabled: false }\n\
             steps: { plan: {}, build: { agent: codex } }\n\
             sequence: { steps: [plan, build], limits: { cost: 5.0 } }\n",
        );
        let err = check(&cfg, &[script_judge("g")]).unwrap_err().to_string();
        assert!(err.contains("build"), "must name the codex STEP:\n{err}");
        assert!(err.contains("codex"), "must name the agent:\n{err}");
        assert!(!err.contains("step `plan`"), "the claude step reports dollars — it must NOT be flagged:\n{err}");
    }
}
