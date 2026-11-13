//! An exam step (A-37): the result of earlier steps is its own check: a fresh examinee answers from
//! what it may read, the step's checks, the answers' check and a grader judge the round, and a repairer fixes what they
//! found, round after round, until the exam passes or a round finds what the one before found.

use std::collections::BTreeSet;

use anyhow::anyhow;

use super::exec::Ctx;
use super::format::Worker;
use super::proc::Proc;
use crate::core::model::{Judge, JudgeKind, Verdict};
use crate::driver::{Fatal, GateOutcome};
use crate::workflow::{commit, with_lead, with_section, When};

/// Rounds an exam that sets none runs at most.
const ROUNDS: usize = 5;

/// What the repairer's round came to.
enum Repair {
    Changed,
    Unchanged,
    /// Findings the repairer disputes: a decision or the code says otherwise than the exam.
    Disputed(String),
}

impl Proc {
    /// Runs the exam step of `ctx` until it passes; a round that finds what the one before found, or a repair that
    /// changes nothing, fails the step, and the task force looks for its origin.
    pub fn exam(&self, ctx: &Ctx) -> Result<(), Fatal> {
        let label = &ctx.label;
        if !self.runs(label)? {
            return Ok(());
        }
        if let Err(why) = self.check(label, When::Before) {
            return self.fail(label, &why);
        }
        let rounds = ctx.step.rounds.unwrap_or(ROUNDS);
        let ids = self.exam_ids(ctx);
        let mut last = None;
        for round in 1..=rounds {
            let verdict = self.exam_judged(ctx)?;
            if verdict.met() {
                self.record(label, "done", true, verdict.rationale());
                return Ok(());
            }
            let found = findings(verdict.rationale(), &ids);
            if last.as_ref() == Some(&found) || round == rounds {
                return self.fail(label, &format!("the repair didn't get past this (round {round}): {}", verdict.rationale()));
            }
            self.record(label, "repair", false, verdict.rationale());
            match self.exam_repaired(ctx, verdict.rationale())? {
                Repair::Disputed(why) => return self.exam_disputed(ctx, &why),
                Repair::Unchanged => return self.fail(label, &format!("the repair changed nothing: {}", verdict.rationale())),
                Repair::Changed => {}
            }
            last = Some(found);
        }
        Ok(())
    }

    /// The repairer found the exam wrong (it expects what a decision or the code contradicts): the step `dispute.reenter`
    /// writes it again with that finding, once; a second dispute goes to the task force.
    fn exam_disputed(&self, ctx: &Ctx, why: &str) -> Result<(), Fatal> {
        let what = format!("the exam expects what a decision or the code contradicts, so the exam is wrong, not what it examines: {why}");
        let Some(target) = ctx.step.dispute.as_ref().map(|d| d.reenter.clone()) else {
            return self.fail(&ctx.label, &what);
        };
        if self.run.from.as_deref() == Some(target.as_str()) {
            return self.fail(&ctx.label, &format!("{what} (after `{target}` wrote the exam again)"));
        }
        self.reenter(&target, &format!("{}: {what}", ctx.label))
    }

    /// One round's verdict: the checks, then the examinee on a fresh copy, its hooks, the answers, the grader.
    fn exam_judged(&self, ctx: &Ctx) -> Result<Verdict, Fatal> {
        for check in &ctx.step.checks {
            let verdict = self.judge_now(&self.cached(&self.check_to_judge(ctx, check)?)?);
            if !verdict.met() {
                return Ok(verdict);
            }
        }
        // an earlier round's answers, left in place, are kept as they are instead of answered anew
        let stale: Vec<&str> = ctx.step.fresh.iter().map(String::as_str).filter(|f| self.project.join(f).exists()).collect();
        for f in &stale {
            std::fs::remove_file(self.project.join(f)).map_err(|e| anyhow!("{f}: {e}"))?;
        }
        if !stale.is_empty() {
            commit(&self.project, &stale, &format!("{}: a new round answers anew", ctx.label))?;
        }
        if let Some(why) = self.examined(ctx)? {
            return Ok(Verdict::binary(false).with_rationale(why));
        }
        let answers = self.judge(ctx, Some(&format!("{}-answers", ctx.key)), ctx.step.answers.as_deref().unwrap_or_default())?;
        let (tasks, questions) = (self.judge_now(&self.cached(&answers)?), self.judge_now(&self.grader(ctx)?));
        if tasks.met() && questions.met() {
            return Ok(questions);
        }
        let found: Vec<&str> = [&tasks, &questions].into_iter().filter(|v| !v.met()).map(|v| v.rationale()).collect();
        Ok(Verdict::binary(false).with_rationale(found.join("\n")))
    }

    /// The grader: a rubric judge on the critic role, with the step's critic settings.
    fn grader(&self, ctx: &Ctx) -> Result<Judge, Fatal> {
        let s = self.rendered(ctx, self.settings.for_step(&ctx.key, ctx.id()))?;
        let roles = self.roles.as_ref().ok_or_else(|| anyhow!("{}: an exam needs the process's `roles`", ctx.label))?;
        let rubric = self.home.join(ctx.step.grader.as_deref().unwrap_or_default());
        Ok(s.critic_of(roles.judge("critic", &format!("{}-grader", ctx.key), rubric))?)
    }

    /// The examinee's round, its result held to the step's hooks: a result that fails them is the examinee's own to fix,
    /// once, never the repairer's. The hooks' finding, if they still fail.
    fn examined(&self, ctx: &Ctx) -> Result<Option<String>, Fatal> {
        let examinee = self.worker_of(ctx, "examinee", ctx.step.examinee.as_ref())?;
        let run = |step: &crate::driver::Step| -> Result<Option<String>, Fatal> {
            self.step(&self.standing(ctx, step)?)?;
            self.gate()?;
            Ok(self.check(&ctx.label, When::After).err())
        };
        let Some(why) = run(&examinee)? else { return Ok(None) };
        self.record(&ctx.label, "repair", false, &why);
        run(&with_lead(&examinee, &format!("## Your result fails its checks\n\nYou answered the exam before; your answers \
            are in place, but they fail these checks. Fix exactly this, and change no answer:\n\n{why}"))?)
    }

    /// The repairer fixes what the round found, or disputes a finding that a decision contradicts.
    fn exam_repaired(&self, ctx: &Ctx, found: &str) -> Result<Repair, Fatal> {
        let disputes = self.scratch(&format!("{}-disputes.md", ctx.key))?;
        let _ = std::fs::remove_file(&disputes);
        let mut judges = vec![];
        for check in &ctx.step.checks {
            judges.push(self.check_to_judge(ctx, check)?);
        }
        judges.push(self.judge(ctx, Some(&format!("{}-answers", ctx.key)), ctx.step.answers.as_deref().unwrap_or_default())?);
        let listed: Vec<String> = judges.iter()
            .filter_map(|j| match &j.kind { JudgeKind::Script { path } => Some(format!("- `{}`", path.display())), _ => None })
            .collect();
        let repairer = self.worker_of(ctx, "repairer", ctx.step.repairer.as_ref())?;
        let step = with_lead(&repairer, &format!("## What the exam found\n\n{found}\n\n## The checks\n\nEach prints its \
            verdict as its last line; the last one reruns the examinee's task commands, so you can see a failure for \
            yourself. Run them before you finish:\n\n{}\n\nThe disputes file: `{}`.", listed.join("\n"), disputes.display()))?;
        self.step(&self.standing(ctx, &step)?)?;
        let kept = matches!(self.gate()?, GateOutcome::Kept);
        let disputed = std::fs::read_to_string(&disputes).unwrap_or_default();
        Ok(match (disputed.trim(), kept) {
            ("", true) => Repair::Changed,
            ("", false) => Repair::Unchanged,
            (why, _) => Repair::Disputed(why.to_string()),
        })
    }

    /// A worker of the exam (`examinee`, `repairer`): its prompt with its sections, its write limits, the step's
    /// worker settings.
    fn worker_of(&self, ctx: &Ctx, role: &str, w: Option<&Worker>) -> Result<crate::driver::Step, Fatal> {
        let w = w.ok_or_else(|| anyhow!("{}: an exam needs `{role}`", ctx.label))?;
        let roles = self.roles.as_ref().ok_or_else(|| anyhow!("{}: an exam needs the process's `roles`", ctx.label))?;
        let s = self.rendered(ctx, self.settings.for_step(&ctx.key, ctx.id()))?;
        let mut step = roles.step("worker", &format!("{}-{role}", ctx.key), self.home.join(&w.prompt).display().to_string())
            .write_deny(self.deny_globs(ctx, &ctx.step.deny)?).write_deny(self.deny_globs(ctx, &w.deny)?);
        for section in &w.sections {
            step = with_section(&step, &self.section_text(ctx, section)?)?;
        }
        s.worker_of(step).map_err(Fatal::from)
    }

    /// The sections every worker's brief gets (`run.brief.sections`), after its own.
    fn standing(&self, ctx: &Ctx, step: &crate::driver::Step) -> Result<crate::driver::Step, Fatal> {
        let mut step = step.clone();
        for section in &self.shared.spec.run.brief.sections {
            step = with_section(&step, &self.section_text(ctx, section)?)?;
        }
        Ok(step)
    }

    /// The ids of the exam's tasks and questions (`findings_ids`), to tell a round's findings apart.
    fn exam_ids(&self, ctx: &Ctx) -> BTreeSet<String> {
        let Some(spec) = &ctx.step.findings_ids else { return BTreeSet::new() };
        let exam: serde_yaml::Value = std::fs::read_to_string(self.project.join(&spec.file)).ok()
            .and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
        spec.lists.iter().filter_map(|k| exam.get(k.as_str()).and_then(|v| v.as_sequence())).flatten()
            .filter_map(|i| i.get("id").and_then(|v| v.as_str()).map(String::from)).collect()
    }
}

/// What a round found, to compare with the next: the exam's ids its rationale names, else the rationale.
fn findings(rationale: &str, ids: &BTreeSet<String>) -> BTreeSet<String> {
    let named: BTreeSet<String> = rationale.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| ids.contains(*w)).map(String::from).collect();
    if named.is_empty() { BTreeSet::from([rationale.to_string()]) } else { named }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A round's findings are the exam ids it names, so a reworded verdict on the same answers is a stall; a verdict
    /// naming none is compared as it stands.
    #[test]
    fn findings_are_the_exam_ids_a_verdict_names() {
        let ids: BTreeSet<String> = ["t1", "q-2"].map(String::from).into();
        assert_eq!(findings("t1 fails; q-2 unanswered", &ids), ids);
        assert_eq!(findings("the task t1 broke", &ids), BTreeSet::from(["t1".to_string()]));
        assert_eq!(findings("nothing named", &ids), BTreeSet::from(["nothing named".to_string()]));
    }
}
