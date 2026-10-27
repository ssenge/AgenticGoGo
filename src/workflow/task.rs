//! An agent step with its judges (A-26): what the worker does, the checks and the critic that judge it, the
//! repair rounds, a script that does what it can first, and what the step reads.

use crate::core::model::Judge;
use crate::driver::Step;

/// An agent step for [`super::Flow::task`]: built with [`Task::new`] and the setters below.
#[derive(Clone)]
pub struct Task {
    /// the step's name in the flow (its key is the first word); none: the worker step's own name.
    pub name: Option<String>,
    /// the worker's step: its agent and task.
    pub step: Step,
    /// sections the worker's brief gets after the task, which the critic doesn't see (inputs, house rules).
    pub sections: Vec<String>,
    /// script judges, in order: the first unmet one is the verdict, and the critic runs only when all are met.
    pub checks: Vec<Judge>,
    /// the LLM judge that reviews the result after the checks.
    pub critic: Option<Judge>,
    /// what the critic gets below its rubric besides the worker's task: what it reviews and what it never raises.
    pub critic_context: String,
    /// repair rounds after the first session.
    pub repairs: usize,
    /// a script that does what it can before the worker (verdict: met when nothing is left).
    pub first: Option<Judge>,
    /// what the step reads (git pathspecs): a refresh with none of them changed needs no session.
    pub inputs: Option<Vec<String>>,
    /// an unmet end returns [`Outcome::Unmet`] to the caller, the work kept, instead of failing into the task force.
    pub climbs: bool,
}

impl Task {
    /// The worker's step, with no judges, no repairs and nothing else.
    pub fn new(step: Step) -> Task {
        Task { name: None, step, sections: vec![], checks: vec![], critic: None, critic_context: String::new(), repairs: 0,
               first: None, inputs: None, climbs: false }
    }

    /// The step's name in the flow, when it isn't the worker step's own (e.g. `"2.2 Derive goals"` for a
    /// worker step named after its role).
    pub fn named(mut self, name: impl Into<String>) -> Task {
        self.name = Some(name.into());
        self
    }

    /// A section of the worker's brief after the task, unseen by the critic.
    pub fn section(mut self, text: impl Into<String>) -> Task {
        self.sections.push(text.into());
        self
    }

    /// One more check, after the ones set.
    pub fn check(mut self, judge: Judge) -> Task {
        self.checks.push(judge);
        self
    }

    /// The critic.
    pub fn critic(mut self, judge: Judge) -> Task {
        self.critic = Some(judge);
        self
    }

    /// What the critic gets below its rubric besides the worker's task.
    pub fn critic_context(mut self, text: impl Into<String>) -> Task {
        self.critic_context = text.into();
        self
    }

    /// Repair rounds after the first session.
    pub fn repairs(mut self, n: usize) -> Task {
        self.repairs = n;
        self
    }

    /// The script that does what it can before the worker.
    pub fn first(mut self, script: Judge) -> Task {
        self.first = Some(script);
        self
    }

    /// What the step reads.
    pub fn inputs<I: IntoIterator<Item = S>, S: Into<String>>(mut self, paths: I) -> Task {
        self.inputs = Some(paths.into_iter().map(Into::into).collect());
        self
    }


    /// An unmet end returns to the caller (who climbs to a next attempt) instead of failing into the task force.
    pub fn climbs(mut self) -> Task {
        self.climbs = true;
        self
    }

    /// The step's name in the flow.
    pub fn name(&self) -> String {
        self.name.clone().or_else(|| self.step.name.clone()).unwrap_or_default()
    }
}

/// What an agent step came to, when it didn't fail (a failed step goes to the task force or ends the run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// the run didn't reach the step (`--from` is later).
    NotReached,
    /// its judges are met and its work kept.
    Done,
    /// a refresh that had nothing to change.
    Unchanged,
    /// the checks are met, the critic still sees this point after its repair round; the work is kept, and
    /// the point is for the humans at the next approval.
    Accepted(String),
    /// a task that [`Task::climbs`] ended unmet, its work kept: why, for the caller's next attempt.
    Unmet(String),
}
