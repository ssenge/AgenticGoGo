//! An agent step's run (A-26, A-29): refresh or not, a script first, the brief, the session, then the checks and
//! the critic, and repair rounds until they are met, the worker can't fix it, or the critic's round is spent.
//! Every session starts fresh: what a step knows is in its brief and the project's files, never in an earlier
//! session.

use super::brief::{with_lead, with_section};
use super::flow::{Flow, When};
use super::git::{changes_since, unchanged_since};
use super::review::{cannot_fix, focused, self_checked, stuck, with_rubric, with_task, Earlier};
use super::task::{Outcome, Task};
use super::key;
use crate::core::model::{Judge, Verdict};
use crate::driver::{Fatal, GateOutcome, Landing, Step};

/// Repair rounds a critic's points get: what it still sees after that is for the humans at the next approval.
const CRITIC_ROUNDS: usize = 1;
/// How much of the changes since its result a refreshing step gets in its brief.
const DIFF_LIMIT: usize = 12_000;

/// The judges of one step: the after-hooks and checks in order, then the critic.
struct Judges<'a> {
    flow: &'a Flow,
    name: &'a str,
    checks: &'a [Judge],
    critic: Option<&'a Judge>,
}

impl Judges<'_> {
    /// The step's verdict, and whether it is the critic's; `earlier` narrows the critic to its last points.
    fn review(&self, earlier: Option<&Earlier>) -> Result<(Verdict, bool), Fatal> {
        if let Err(why) = self.flow.check(self.name, When::After) {
            return Ok((Verdict::binary(false).with_rationale(why), false));
        }
        let mut last = None;
        for check in self.checks {
            let verdict = self.flow.judge(check);
            if !verdict.met() {
                return Ok((verdict, false));
            }
            last = Some(verdict);
        }
        let Some(critic) = self.critic else {
            let taken = || Verdict::binary(true).with_rationale("no critic: the checks judge it alone");
            return Ok((last.unwrap_or_else(taken), false));
        };
        let critic = match earlier.filter(|e| e.by_critic) {
            Some(e) => focused(self.flow, critic, e)?,
            None => critic.clone(),
        };
        Ok((self.flow.judge(&critic), true))
    }
}

/// What a step's `first` script left for its worker.
enum First {
    None,
    Done,
    Rest(String),
}

impl Flow {
    /// Runs an agent step (A-26) and says what it came to; a failed step goes to the task force or ends the run.
    pub fn task(&self, t: &Task) -> Result<Outcome, Fatal> {
        let name = t.name();
        if !self.runs(&name)? {
            return Ok(Outcome::NotReached);
        }
        // only a step that names what it reads can tell a refresh from a new task: one without inputs runs it whole
        let since = t.inputs.as_ref().and_then(|_| self.refreshing(&name));
        if let Some(why) = since.as_deref().and_then(|c| untouched(self, t, c)) {
            self.record(&name, "unchanged", true, &why);
            return Ok(Outcome::Unchanged);
        }
        if let Err(why) = self.check(&name, When::Before) {
            self.recover(&name, &why, &|| Ok(self.check(&name, When::Before).err()))?;
        }
        let step = self.absolute(&t.step);
        let critic = t.critic.as_ref().map(|c| with_task(self, &self.absolute_judge(c), &t.critic_context, &step))
            .transpose()?;
        let judges = Judges { flow: self, name: &name, checks: &t.checks, critic: critic.as_ref() };
        let base = self.lead(&name, &step, since.as_deref(), t.inputs.as_deref().unwrap_or_default())?;
        let base = match self.first(&name, t, &judges)? {
            First::Done => return Ok(Outcome::Done),
            First::Rest(rest) => with_section(&base, &rest)?,
            First::None => base,
        };
        // a refresh adjusts a result the discussion already shaped
        let base = if since.is_none() { self.discussed(&name, &base, t)? } else { base };
        let cannot = self.scratch(&format!("{}-cannot-fix.md", key(&name)))?;
        let rubric = t.critic.as_ref().map(|c| self.absolute_judge(c));
        let step = self.briefed(&name, &base, t, rubric.as_ref(), &cannot)?;
        let landed = self.step(&step)?.landed;
        if landed == Landing::Nothing && since.is_some() {
            self.gate()?;
            self.record(&name, "unchanged", true, "refreshed: the changes since its last result didn't concern it");
            return Ok(Outcome::Unchanged);
        }
        // a fault a later step found must change the result; a decision may not concern it (then it refreshes)
        if landed == Landing::Nothing && self.run.from.as_deref() == Some(key(&name)) && self.run.note.is_some()
            && !self.run.decided {
            return self.unmet(&name, t.climbs, "the re-entered step changed nothing, so its note wasn't carried out",
                              Some(&judges));
        }
        self.repaired(&name, &step, (t.repairs, t.climbs), &judges, &cannot)
    }

    /// Whether step `name` refreshes an earlier result: the run re-entered before it and keeps the later
    /// results, or re-entered at it with a decision. The commit its last result stands on, if it has one.
    pub(crate) fn refreshing(&self, name: &str) -> Option<String> {
        let at = self.run.from.as_deref() == Some(key(name));
        let reentered = self.run.from.is_some() && !self.run.reset && (!at || self.run.decided);
        if reentered { self.steps().last_commit(name, &["done", "unchanged"]) } else { None }
    }

    /// The step with what is new leading its brief: the changes to its `inputs` since its result, or the note at
    /// `--from`.
    fn lead(&self, name: &str, step: &Step, since: Option<&str>, inputs: &[String]) -> Result<Step, Fatal> {
        if let Some(commit) = since {
            let mut skip = vec!["agg".to_string()];
            skip.extend(self.own.iter().cloned());
            let (stat, diff) = changes_since(self.project(), commit, inputs, &skip, DIFF_LIMIT);
            return with_lead(step, &format!(
                "## Since your last result\n\nYou did this task before, and your result is in place. Since then, \
                 these changes were made to what it reads:\n\n```\n{stat}\n```\n\n```diff\n{diff}\n```\n\nAdjust your \
                 result only where these changes require it, and change nothing else. If they don't concern your \
                 result, change nothing at all and finish."));
        }
        match &self.run.note {
            Some(note) if self.run.from.as_deref() == Some(key(name)) && self.run.decided => with_lead(step, &format!(
                "## Decided since your result\n\nYou did this task before, and your result is in place. Since then, \
                 humans decided what follows. Adjust your result only where it requires, and change nothing else. \
                 If it doesn't concern your result, change nothing at all and finish.\n\n{note}")),
            Some(note) if self.run.from.as_deref() == Some(key(name)) => with_lead(step, &format!(
                "## Note from a later step\n\nYou already did this task; a later step found the problem below in \
                 your result. Fix it, and change nothing else.\n\n{note}")),
            _ => Ok(step.clone()),
        }
    }

    /// Runs the step's `first` script: what it wrote is committed; when it leaves nothing and the judges are
    /// met, the step is done without a session; otherwise the worker gets what is left.
    fn first(&self, name: &str, t: &Task, judges: &Judges) -> Result<First, Fatal> {
        let Some(script) = &t.first else { return Ok(First::None) };
        let verdict = self.judge(script);
        super::git::commit(self.project(), &["."], &format!("{name}: its script"))?;
        let rest = |said: &str| First::Rest(format!(
            "## What a script did\n\nA script did what it could and wrote it into this step's results: keep it, \
             and do only what is left. It says: {said}"));
        if let Some(error) = verdict.error() {
            return Ok(rest(&format!("it failed, so all of it is yours: {error}")));
        }
        if !verdict.met() {
            return Ok(rest(verdict.rationale()));
        }
        let (judged, _) = judges.review(None)?;
        if !judged.met() {
            return Ok(rest(&format!("{}; but the check says: {}", verdict.rationale(), judged.rationale())));
        }
        self.record(name, "done", true, &format!("without a session: {}; {}", verdict.rationale(), judged.rationale()));
        self.flush()?;
        Ok(First::Done)
    }

    /// The worker's brief: the task and what leads it, the driver's sections, the critic's rubric (`critic` as
    /// written: the task it would repeat is already above), the checks to run, and the way out when it can't
    /// fix a failure.
    fn briefed(&self, name: &str, base: &Step, t: &Task, critic: Option<&Judge>, cannot: &std::path::Path)
               -> Result<Step, Fatal> {
        let mut step = base.clone();
        for section in &t.sections {
            step = with_section(&step, section)?;
        }
        if let Some(critic) = critic {
            step = with_rubric(&step, critic)?;
        }
        let checks: Vec<Judge> = self.hook_judges(name, When::After).into_iter().chain(t.checks.iter().cloned()).collect();
        with_section(&self_checked(&step, &checks)?, &cannot_fix(cannot))
    }

    /// Judge, repair, judge again: up to `repairs` rounds, one of them the critic's.
    fn repaired(&self, name: &str, step: &Step, (repairs, climbs): (usize, bool), judges: &Judges,
                cannot: &std::path::Path)
                -> Result<Outcome, Fatal> {
        let (mut verdict, mut by_critic) = judges.review(None)?;
        let (mut previous, mut critic_rounds): (Option<String>, usize) = (None, 0);
        for round in 1..=repairs {
            if verdict.met() || (by_critic && critic_rounds == CRITIC_ROUNDS) {
                break;
            }
            // what blocks the check lies in another step's result: no repair round of this worker's can move it
            if let Some(blocker) = &verdict.blocked_by {
                return self.unmet(name, climbs, &format!("blocked by {blocker}: {}", verdict.rationale()),
                                  Some(judges));
            }
            critic_rounds += usize::from(by_critic);
            if let Some(why) = stuck(cannot, verdict.rationale(), by_critic, previous.as_deref()) {
                return self.unmet(name, climbs, &why, Some(judges));
            }
            previous = Some(verdict.rationale().to_string());
            (verdict, by_critic) = self.repair(name, step, round, (&verdict, by_critic), judges)?;
        }
        // a worker given no repair round can't answer the critic: its point lies in an earlier step's result,
        // which the task force finds; after a worker tried, what the critic still sees is for the next approval
        if !verdict.met() && by_critic && repairs > 0 {
            return self.accepted(name, verdict.rationale());
        }
        if !verdict.met() {
            return self.unmet(name, climbs, verdict.rationale(), Some(judges));
        }
        self.keep(name)?;
        self.record(name, "done", true, verdict.rationale());
        Ok(Outcome::Done)
    }

    /// One repair round: a fresh session fixes the points (or disputes one), then the review.
    fn repair(&self, name: &str, step: &Step, round: usize, (verdict, by_critic): (&Verdict, bool), judges: &Judges)
              -> Result<(Verdict, bool), Fatal> {
        self.record(name, "repair", false, verdict.rationale());
        let disputed = self.scratch(&format!("{}-disputes-{round}.md", key(name)))?;
        self.step(&with_lead(step, &format!(
            "## Review to address\n\nYou already did this task; a reviewer checked your result and found the \
             points below. Fix each point that is right, and change nothing else. If you are sure a point is \
             wrong, leave it unchanged and write the point with your reason, in one sentence, to {}. A point \
             the reviewer repeats after reading your reason must be fixed: the reviewer has weighed your \
             reason and still sees the issue.\n\n{}", disputed.display(), verdict.rationale()))?)?;
        let disputes = std::fs::read_to_string(&disputed).unwrap_or_default();
        judges.review(Some(&Earlier { round, points: verdict.rationale().to_string(), disputes, by_critic }))
    }

    /// The critic still sees a point after its round: the work is kept, the point recorded and returned.
    fn accepted(&self, name: &str, point: &str) -> Result<Outcome, Fatal> {
        self.info(&format!("{name}: kept; the reviewer's open point is for the next approval: {point}"));
        self.keep(name)?;
        self.record(name, "accepted", false, point);
        self.record(name, "done", true, point);
        Ok(Outcome::Accepted(point.to_string()))
    }

    /// Lands the step's work on the base branch.
    fn keep(&self, name: &str) -> Result<(), Fatal> {
        match self.gate()? {
            GateOutcome::Kept | GateOutcome::Nothing => Ok(()),
            other => self.fail(name, &format!("the gate did not keep the work ({other:?})")),
        }
    }

    /// An unmet end: returned to a caller that climbs, the work kept for its next attempt; else a failure,
    /// which `judges` judge again after a repair in place.
    fn unmet(&self, name: &str, climbs: bool, why: &str, judges: Option<&Judges>) -> Result<Outcome, Fatal> {
        if !climbs {
            return self.failed(name, why, judges);
        }
        self.keep(name)?;
        self.record(name, "unmet", false, why);
        Ok(Outcome::Unmet(why.to_string()))
    }

    /// Fails the step (the task force, or the run's end); repaired in place and met when `judges` judge it again, the
    /// step is done.
    fn failed(&self, name: &str, why: &str, judges: Option<&Judges>) -> Result<Outcome, Fatal> {
        let Some(judges) = judges else {
            self.fail(name, why)?;
            return Ok(Outcome::NotReached);
        };
        let again = || judges.review(None).map(|(v, _)| (!v.met()).then(|| v.rationale().to_string()));
        self.recover(name, why, &again)?;
        self.keep(name)?;
        self.record(name, "done", true, "met after the task force's repair in place");
        Ok(Outcome::Done)
    }
}

/// Why a refreshing step needs no session: none of its declared inputs changed since `commit`.
fn untouched(flow: &Flow, t: &Task, commit: &str) -> Option<String> {
    let inputs = t.inputs.as_ref()?;
    unchanged_since(flow.project(), commit, inputs)
        .then(|| format!("refreshed without a session: none of its inputs changed ({})", inputs.join(", ")))
}
