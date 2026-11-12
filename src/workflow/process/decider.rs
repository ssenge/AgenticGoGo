//! The `agent` tier of a ladder (A-37): before each further attempt a cheap agent, the
//! decider, picks the next tier from the step's history, within what the step allows; the run checks its pick. It
//! decides about effort (mode, model, repairs, a tool), never about checks, critics or gates.

use anyhow::anyhow;
use serde::Deserialize;

use super::ladder::{AgentTier, Mode, TierSpec};
use super::proc::Proc;
use super::tiers::Ladder;
use crate::driver::{Effort, Fatal, Step};

/// The tier taken when a pick breaks a rule: the strongest an agent tier gets without a choice.
const FALLBACK: AgentTier = AgentTier { mode: Mode::Converge, model: None, effort: None, agent: None, repairs: None, max: None };

/// What the decider may pick for one step: its modes, the project's model sizes, its tool tiers.
pub struct Allowed {
    pub modes: Vec<Mode>,
    pub models: Vec<String>,
    pub tools: Vec<String>,
}

/// The decider's answer file.
#[derive(Deserialize, Default)]
struct Answer {
    tier: Option<TierSpec>,
    stop: Option<String>,
    why: Option<String>,
}

/// What the step may pick: every mode its judges support, the project's sizes, and on an item of an `each` the
/// process's tools plus the ones the project enabled.
pub fn allowed(p: &Proc, l: &Ladder) -> Allowed {
    let modes = if l.critic.is_some() { vec![Mode::Once, Mode::Dccr, Mode::Converge] } else { vec![Mode::Once, Mode::Converge] };
    let tools = if l.ctx.item.is_some() {
        p.shared.spec.ladder.iter().flat_map(|s| s.tools.clone()).chain(p.settings.managed()).collect()
    } else {
        vec![]
    };
    Allowed { modes, models: p.settings.sizes(), tools }
}

/// The decider's pick for the next attempt: a tier and why, or None when it says stop. A pick that breaks a rule is
/// replaced by the fallback, and the reason says so.
pub fn pick(p: &Proc, l: &Ladder, ok: &Allowed, last: &str) -> Result<Option<(TierSpec, String)>, Fatal> {
    let (label, key) = (&l.ctx.label, &l.ctx.key);
    let file = p.scratch(&format!("{key}-decider.yaml"))?;
    let _ = std::fs::remove_file(&file);
    let mut step = Step::new("tier-decider").agent(l.base.step.agent.clone()).effort(Effort::Low)
        .prompt(brief(p, l, ok, last, &file.display().to_string())).write_deny(["**"]);
    // the cheapest size the project names, `small` when it has one
    if let Some(size) = ok.models.iter().find(|m| *m == "small").or(ok.models.first()) {
        step = step.model(p.settings.model(size).as_str());
    }
    p.step(&step)?;
    p.gate()?;
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let answer: Answer = serde_yaml::from_str(&text).unwrap_or_default();
    if let Some(why) = answer.stop {
        p.record(label, "tier", false, &format!("the decider stops: {why}"));
        return Ok(None);
    }
    let why = answer.why.unwrap_or_default();
    Ok(Some(match answer.tier.filter(|r| valid(r, ok)) {
        Some(tier) => (tier, why),
        None => (TierSpec::Agent(FALLBACK), format!("the decider's pick broke a rule or was missing ({}); converge \
                                                       instead", text.trim().replace('\n', " "))),
    }))
}

/// Whether a pick keeps to what the step allows.
fn valid(tier: &TierSpec, ok: &Allowed) -> bool {
    match tier {
        TierSpec::Agent(r) => ok.modes.contains(&r.mode) && r.agent.is_none()
            && r.model.as_ref().is_none_or(|m| ok.models.contains(m))
            && r.effort.as_deref().is_none_or(|e| e.parse::<Effort>().is_ok()),
        TierSpec::Tool { tool } => ok.tools.contains(tool),
        TierSpec::Decide(_) => false,
    }
}

/// The decider's brief: the step, its ladder and history, the last verdict, what it may pick, the answer's form.
fn brief(p: &Proc, l: &Ladder, ok: &Allowed, last: &str, file: &str) -> String {
    let name = &l.ctx.label;
    let history: Vec<String> = p.steps().entries().iter()
        .filter(|e| e["step"] == name.as_str() && ["tier", "unmet", "repair", "task-force"].contains(&e["kind"].as_str().unwrap_or("")))
        .map(|e| format!("- {}: {}", e["kind"].as_str().unwrap_or(""), clip(e["rationale"].as_str().unwrap_or(""))))
        .collect();
    let modes: Vec<String> = ok.modes.iter().map(|m| format!("{m:?}").to_lowercase()).collect();
    format!("# Pick the next attempt at step {name}\n\nStep {name} hasn't met its judges yet. You don't do the step; \
        you pick how its next attempt runs, from its history. Read nothing else, change nothing.\n\n\
        ## Its attempts in this run\n\n{h}\n\n## The last verdict\n\n{last}\n\n## What you may pick\n\n\
        - an agent attempt: `tier: {{ mode: <{m}>, model: <{models}>, effort: <low | medium | high>, repairs: <n>, \
        max: <n> }}` (once: its checks only; dccr: checks and critic; converge: repairs until met or stalled, at \
        most max){tools}\n- or `stop: <why>`, when no attempt can meet the judges (the fault is in an earlier \
        step's result, or outside the step).\n\nPick more effort (a stronger model, converge) where the failure \
        needs judgment, never the same tier that failed the same way again.\n\n## Your answer\n\nWrite it to {file} \
        as YAML, with `why:` in one line, like:\n\n```yaml\ntier: {{ mode: converge, model: <a size>, max: 5 }}\n\
        why: two repairs at the small size left the same gate unmet; it needs a larger model\n```\n",
        h = if history.is_empty() { "(none recorded)".into() } else { history.join("\n") },
        last = clip(last), m = modes.join(" | "),
        models = if ok.models.is_empty() { "leave it out: the step's own".into() } else { ok.models.join(" | ") },
        tools = if ok.tools.is_empty() { String::new() }
                else { format!("\n- a tool attempt: `tier: {{ tool: <{}> }}`", ok.tools.join(" | ")) })
}

/// A rationale cut to what a decision needs.
fn clip(text: &str) -> String {
    let t: String = text.chars().take(600).collect();
    if t.len() < text.len() { format!("{t} …") } else { t }
}

/// Refuses a word other than `agent` where a ladder names a decider.
pub fn word(name: &str, w: &str) -> Result<(), Fatal> {
    if w == "agent" { Ok(()) } else { Err(anyhow!("{name}: a tier is `agent`, a map or a tool, not `{w}`").into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Allowed {
        Allowed { modes: vec![Mode::Once, Mode::Converge], models: vec!["small".into()], tools: vec!["transform.recipes".into()] }
    }

    fn tier(yaml: &str) -> TierSpec {
        serde_yaml::from_str(yaml).unwrap()
    }

    /// A pick keeps to what the step allows.
    #[test]
    fn a_pick_keeps_to_what_the_step_allows() {
        assert!(valid(&tier("{ mode: converge, model: small, max: 4 }"), &ok()));
        assert!(valid(&tier("{ tool: transform.recipes }"), &ok()));
        assert!(!valid(&tier("{ mode: dccr }"), &ok()), "a mode the step's judges don't support");
        assert!(!valid(&tier("{ mode: once, model: huge }"), &ok()), "a size the project doesn't name");
        assert!(!valid(&tier("{ mode: once, agent: codex }"), &ok()), "the decider never changes the agent");
        assert!(!valid(&tier("{ tool: other }"), &ok()));
        assert!(!valid(&tier("agent"), &ok()));
    }

    /// The answer is a tier or a stop; a long rationale is clipped.
    #[test]
    fn the_answer_is_a_tier_or_a_stop() {
        let a: Answer = serde_yaml::from_str("tier: { mode: once }\nwhy: cheap\n").unwrap();
        assert!(a.tier.is_some() && a.stop.is_none());
        let a: Answer = serde_yaml::from_str("stop: the fault is upstream\n").unwrap();
        assert_eq!(a.stop.as_deref(), Some("the fault is upstream"));
        assert!(clip(&"x".repeat(700)).ends_with(" …"));
        assert!(word("s", "agent").is_ok() && word("s", "other").is_err());
    }
}
