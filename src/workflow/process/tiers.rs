//! Running a step's ladder (A-37): its tiers, first to last, each working on what the one
//! before left and judged by the step's own checks and critic. The first tier that meets them ends it; an unmet one
//! hands over to the next; the last one goes to the task force as a step always did. A re-entry with a note starts one
//! tier above the one the step reached.

use anyhow::anyhow;

use super::exec::Ctx;
use super::ladder::{AgentTier, Mode, TierSpec};
use super::proc::Proc;
use super::settings::StepSettings;
use crate::core::model::{Judge, Verdict};
use crate::driver::Fatal;
use crate::workflow::{commit, Outcome, Task, When};

/// The start of an unmet tier's reason when its tool applied to nothing.
const NOT_APPLICABLE: &str = "n/a: ";
/// Repair rounds of a `converge` tier that sets no `max`: it ends sooner when it stalls.
const CONVERGE_MAX: usize = 8;
/// The decider's picks at most, before the step goes to the task force.
pub const DECIDE_MAX: usize = 3;

/// A step that runs as a ladder: its worker's task (`base`: the step, its sections and checks), its critic, its
/// settings and its repair rounds where a tier sets none.
pub struct Ladder<'a> {
    pub ctx: &'a Ctx<'a>,
    pub base: Task,
    pub critic: Option<Judge>,
    pub settings: &'a StepSettings,
    pub repairs: usize,
}

impl Proc {
    /// Runs the step's tiers from the one it starts on; what the tier that ended it came to.
    pub fn climb(&self, l: &Ladder) -> Result<Outcome, Fatal> {
        let tiers = tiers(l);
        let mut last: Option<String> = None;
        for (i, tier) in tiers.iter().enumerate().skip(self.start(l, tiers.len())) {
            if let TierSpec::Decide(word) = tier {
                super::decider::word(&l.ctx.label, word)?;
                return self.decide(l, last.unwrap_or_else(|| "no attempt yet".into()), (i, tiers.len()));
            }
            let climbs = i + 1 < tiers.len();
            let outcome = self.attempt(l, tier, last.as_deref(), climbs)?;
            if outcome == Outcome::NotReached {
                return Ok(outcome);
            }
            if tiers.len() > 1 {
                self.record_tier(l, (i, tiers.len()), &outcome, tier);
            }
            match outcome {
                // an agent tier that doesn't climb goes to the task force itself; a tool tier last in the ladder fails here
                Outcome::Unmet(why) if !climbs => return self.ladder_failed(l, &why),
                // a tool that doesn't apply is no attempt the next tier should build on
                Outcome::Unmet(why) if why.starts_with(NOT_APPLICABLE) => {}
                Outcome::Unmet(why) => last = Some(why),
                other => return Ok(other),
            }
        }
        // the last tier doesn't climb: it goes to the task force itself, so a ladder never ends here
        Ok(Outcome::NotReached)
    }

    /// A tool tier that applied no tool is recorded as `n/a`, not as a failed attempt.
    fn record_tier(&self, l: &Ladder, (i, of): (usize, usize), outcome: &Outcome, tier: &TierSpec) {
        let (kind, met, how) = match outcome {
            Outcome::Unmet(why) if why.starts_with(NOT_APPLICABLE) => ("n/a", true, why.clone()),
            Outcome::Unmet(why) => ("tier", false, format!("ended unmet: {why}")),
            _ => ("tier", true, self.met_how(l, tier)),
        };
        self.record(&l.ctx.label, kind, met, &format!("tier {} of {of} {how}", i + 1));
    }

    /// How a tier met: a tool tier says whether the tools did it alone or the worker's sessions finished it.
    fn met_how(&self, l: &Ladder, tier: &TierSpec) -> String {
        let TierSpec::Tool { .. } = tier else { return "met".into() };
        let entries = self.steps().entries();
        let done = entries.iter().rev().find(|e| e["step"] == l.ctx.label.as_str() && e["kind"] == "done");
        met_by(done.and_then(|e| e["rationale"].as_str()).unwrap_or(""))
    }

    /// One attempt on an agent or a tool tier.
    fn attempt(&self, l: &Ladder, tier: &TierSpec, last: Option<&str>, climbs: bool) -> Result<Outcome, Fatal> {
        match tier {
            TierSpec::Agent(r) => self.task(&self.agent_task(l, r, last, climbs)?),
            TierSpec::Tool { tool } => self.tool_tier(l, tool),
            TierSpec::Decide(word) => Err(anyhow!("{}: `{word}` can't be an attempt itself", l.ctx.label).into()),
        }
    }

    /// The `agent` tier (tier `at` of `of`): the decider picks each further attempt, at most `DECIDE_MAX`; after its last
    /// pick, or when it stops, the step goes to the task force.
    fn decide(&self, l: &Ladder, mut last: String, (at, of): (usize, usize)) -> Result<Outcome, Fatal> {
        let ok = super::decider::allowed(self, l);
        for k in 1..=DECIDE_MAX {
            let Some((tier, why)) = super::decider::pick(self, l, &ok, &last)? else { break };
            self.record(&l.ctx.label, "tier", true, &format!("tier {} of {of}, the decider's pick {k}: {tier:?}; {why}", at + 1));
            match self.attempt(l, &tier, Some(&last), true)? {
                Outcome::Unmet(why) => last = why,
                other => return Ok(other),
            }
        }
        self.ladder_failed(l, &last)
    }

    /// The step failed after its last tier: the task force may repair its cause in place, and when the step's checks are
    /// met then, it is done.
    fn ladder_failed(&self, l: &Ladder, why: &str) -> Result<Outcome, Fatal> {
        self.recover(&l.ctx.label, why, &|| {
            let verdict = self.judged(l);
            Ok((!verdict.met()).then(|| verdict.rationale().to_string()))
        })?;
        self.record(&l.ctx.label, "done", true, "met after the task force's repair in place");
        self.flush()?;
        Ok(Outcome::Done)
    }

    /// A tool tier: the tool changes the step's work (`ladder.tool_run`), the change is committed, and the step's
    /// after-hooks and checks judge it; met, the step is done without an agent session.
    fn tool_tier(&self, l: &Ladder, tool: &str) -> Result<Outcome, Fatal> {
        let (ctx, label) = (l.ctx, l.ctx.label.as_str());
        let (Some(item), Some(spec)) = (&ctx.item, &self.shared.spec.ladder) else {
            return Err(anyhow!("{label}: a tool tier runs on an item of an `each`, with the process's `ladder`").into());
        };
        if !self.runs(label)? {
            return Ok(Outcome::NotReached);
        }
        if let Err(why) = self.check(label, When::Before) {
            self.recover(label, &why, &|| Ok(self.check(label, When::Before).err()))?;
        }
        let slug: String = tool.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let command = self.render(ctx, &spec.tool_run)?.replace("{tool}", tool);
        // the item in the name: agg memoizes a verdict by its judge's name, and one item's run isn't another's
        let judge = self.judge(ctx, Some(&format!("tool-tier-{}-{slug}", item.key)), &command)?;
        let ran = self.judge_now(&judge);
        let commits = spec.tool_commits.iter().map(|p| self.render(ctx, p)).collect::<Result<Vec<_>, _>>()?;
        commit_existing(&self.project, &commits, &format!("{label}: {tool}"))?;
        if ran.met() && ran.value == Some(0.0) {
            return Ok(Outcome::Unmet(format!("{NOT_APPLICABLE}{tool}: {}", ran.rationale())));
        }
        if !ran.met() {
            return Ok(Outcome::Unmet(format!("{tool}: {}", ran.rationale())));
        }
        let verdict = self.judged(l);
        if !verdict.met() {
            return Ok(Outcome::Unmet(format!("after {tool} ({}): {}", ran.rationale(), verdict.rationale())));
        }
        if let Some(critic) = &l.critic {
            return self.reviewed(l, critic, &item.key);
        }
        self.record(label, "done", true, &format!("{tool} alone: {}", verdict.rationale()));
        self.flush()?;
        Ok(Outcome::Done)
    }

    /// agg's judge call on a judge (the flow derefs to it; named apart from a step's own `judge` here).
    pub(super) fn judge_now(&self, j: &Judge) -> Verdict {
        crate::driver::Agg::judge(self, j)
    }

    /// The tools' result, its checks met, before the step's critic too: a task with a script first that leaves nothing
    /// to do, so the judges decide without a session when the critic agrees; a point it raises gets the worker's repair
    /// round, and an unmet end climbs on.
    fn reviewed(&self, l: &Ladder, critic: &Judge, unit: &str) -> Result<Outcome, Fatal> {
        let done = self.wrapper(&format!("tool-tier-{unit}-done.sh"),
                                r#"echo '{"met": true, "rationale": "the tools did it, and its checks hold"}'"#)?;
        let task = l.base.clone().first(Judge::script(format!("tool-tier-{unit}-done"), done.as_str()))
            .critic(critic.clone()).repairs(0).climbs();
        self.task(&task)
    }

    /// The step's after-hooks, then its checks in order: the first unmet one, or the last met.
    fn judged(&self, l: &Ladder) -> Verdict {
        if let Err(why) = self.check(&l.ctx.label, When::After) {
            return Verdict::binary(false).with_rationale(why);
        }
        let mut last = Verdict::binary(true).with_rationale("no check");
        for check in &l.base.checks {
            last = self.judge_now(check);
            if !last.met() {
                break;
            }
        }
        last
    }

    /// The tier this attempt starts on: one above the one the step reached when the run re-enters it with a note; else
    /// the first.
    fn start(&self, l: &Ladder, len: usize) -> usize {
        // a decision isn't a failed attempt: the step refreshes against it on the tier it reached
        let reentered = self.run.from.as_deref() == Some(l.ctx.key.as_str()) && self.run.note.is_some() && !self.run.decided;
        if !reentered || len < 2 {
            return 0;
        }
        let entries = self.steps().entries();
        let mut reached = entries.iter()
            .filter(|e| e["step"] == l.ctx.label.as_str() && e["kind"] == "tier")
            .filter_map(|e| reached_tier(&e["rationale"]));
        reached.next_back().map_or(0, |n| n.min(len - 1))
    }

    /// The worker's task on agent tier `r`: its model, effort and agent over the step's; its repair rounds; the critic
    /// unless it runs `once`; the earlier attempt's verdict when there was one.
    fn agent_task(&self, l: &Ladder, r: &AgentTier, last: Option<&str>, climbs: bool) -> Result<Task, Fatal> {
        let mut t = l.base.clone();
        let label = &l.ctx.label;
        if let Some(m) = &r.model {
            t.step = t.step.model(self.settings.model(m).as_str());
        }
        if let Some(e) = &r.effort {
            t.step = t.step.effort(e.parse().map_err(|e: String| anyhow!("{label}: tier effort: {e}"))?);
        }
        if let Some(a) = &r.agent {
            t.step = t.step.agent(a.parse().map_err(|e: String| anyhow!("{label}: tier agent: {e}"))?);
        }
        t.repairs = if r.mode == Mode::Converge { r.max.unwrap_or(CONVERGE_MAX) } else { r.repairs.unwrap_or(l.repairs) };
        t.critic = if r.mode == Mode::Once { None } else { l.critic.clone() };
        if let Some(why) = last {
            t = t.section(format!("## An earlier attempt\n\nAn earlier attempt at this step ended unmet; its work is in \
                place: continue from it, and change nothing that already holds. Its last verdict:\n\n{why}"));
        }
        Ok(if climbs { t.climbs() } else { t })
    }
}

/// The tier's credit by its `done` record: agg's "without a session", or the tool tier's own "… alone".
fn met_by(done: &str) -> String {
    if done.starts_with("without a session") || done.split(':').next().is_some_and(|s| s.ends_with(" alone")) {
        "met by the tools alone".into()
    } else {
        "met by the worker's sessions on what the tools left".into()
    }
}

/// The step's tiers: its ladder, or one agent tier as the step ran before ladders (with its critic: dccr).
fn tiers(l: &Ladder) -> Vec<TierSpec> {
    l.settings.ladder.clone().unwrap_or_else(|| {
        vec![TierSpec::Agent(AgentTier { mode: if l.critic.is_some() { Mode::Dccr } else { Mode::Once }, ..Default::default() })]
    })
}

/// Commits what changed under those of `paths` that exist: git refuses a pathspec that matches nothing, and a tier on
/// which no tool applies writes no report. None existing, nothing is committed.
fn commit_existing(project: &std::path::Path, paths: &[String], message: &str) -> anyhow::Result<bool> {
    let there: Vec<&str> = paths.iter().map(String::as_str).filter(|p| project.join(p).exists()).collect();
    if there.is_empty() {
        return Ok(false);
    }
    commit(project, &there, message)
}

/// The tier number (1-based: the index of the next tier) a `tier` record names, `tier 2 of 3 …`.
fn reached_tier(rationale: &serde_json::Value) -> Option<usize> {
    rationale.as_str()?.strip_prefix("tier ")?.split(' ').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only existing paths are committed: git refuses a pathspec that matches nothing.
    #[test]
    fn only_existing_paths_are_committed() {
        let dir = tempfile::tempdir().unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@t"]] {
            std::process::Command::new("git").arg("-C").arg(dir.path()).args(args).status().unwrap();
        }
        std::fs::create_dir_all(dir.path().join("code")).unwrap();
        std::fs::write(dir.path().join("code/a"), "a").unwrap();
        assert!(commit_existing(dir.path(), &["code".into(), "reports/recipes".into()], "m").unwrap());
        assert!(!commit_existing(dir.path(), &["reports/recipes".into()], "m").unwrap(), "none of them there");
    }

    /// A tool tier is credited alone only without a session; the tier reached is read from its record.
    #[test]
    fn a_tool_tier_is_credited_alone_only_without_a_session() {
        assert_eq!(met_by("without a session: x"), "met by the tools alone");
        assert_eq!(met_by("recipes alone: ok"), "met by the tools alone");
        assert!(met_by("the checks hold").starts_with("met by the worker's sessions"));
        assert_eq!(reached_tier(&serde_json::json!("tier 2 of 3 met")), Some(2));
        assert_eq!(reached_tier(&serde_json::json!("done")), None);
    }
}
