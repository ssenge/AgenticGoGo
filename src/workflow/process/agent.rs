//! An agent step (A-37): its worker, its prompt and the sections of its brief, its checks and its
//! critic, run as a ladder (one tier unless the settings say more), the critic's open point kept for the next
//! checkpoint.

use std::path::PathBuf;

use anyhow::anyhow;

use super::exec::Ctx;
use super::format::{Check, Review, Section};
use super::proc::Proc;
use super::settings::StepSettings;
use super::tiers::Ladder;
use super::{bases, inputs, rerun};
use crate::core::model::Judge;
use crate::driver::{Fatal, Step};
use crate::metrics::{Scope, ScoreJudge};
use crate::workflow::{Outcome, Task};

/// Repair rounds of a step that sets none.
const REPAIRS: usize = 2;

impl Proc {
    /// Runs the agent step of `ctx`: the worker's task through its ladder, judged by the checks and the critic.
    pub fn agent(&self, ctx: &Ctx) -> Result<(), Fatal> {
        let s = self.rendered(ctx, self.settings.for_step(&ctx.key, ctx.id()))?;
        s.second_opinion(&ctx.key)?;
        let roles = self.roles.as_ref().ok_or_else(|| anyhow!("{}: an agent step needs the process's `roles`", ctx.label))?;
        let mut step = roles.step("worker", &ctx.key, self.prompt_path(ctx).display().to_string());
        step = step.write_deny(self.deny_globs(ctx, &ctx.step.deny)?);
        let (mut checks, mut extras) = (vec![], vec![]);
        for section in &ctx.step.sections {
            extras.push(self.section_text(ctx, section)?);
        }
        if let Some((score, guide)) = self.quality(ctx)? {
            checks.push(score);
            extras.push(guide);
        }
        if !extras.is_empty() {
            let text = crate::workflow::task(&step)?;
            step = step.prompt(format!("{text}\n\n{}", extras.join("\n\n")));
        }
        for check in &ctx.step.checks {
            if let Some(j) = self.check_judge(ctx, check)? {
                checks.push(j);
            }
        }
        let rubric = self.rubric(ctx);
        let critic = match (&rubric, s.no_critic()) {
            (Some(path), false) => Some(s.critic_of(roles.judge("critic", &format!("{}-review", ctx.key), path.clone()))?),
            _ => None,
        };
        let mut base = self.build_task(ctx, &s.worker_of(step)?, &s)?.critic_context(self.critic_brief(ctx, &s)?);
        for check in &checks {
            base = base.check(self.cached(check)?);
        }
        let repairs = s.repairs.or(ctx.step.repairs).unwrap_or(REPAIRS);
        let reviewed = critic.is_some();
        match self.climb(&Ladder { ctx, base, critic, settings: &s, repairs })? {
            Outcome::Accepted(point) if reviewed => self.open_point(&ctx.label, Some(&point)),
            Outcome::Done if reviewed => self.open_point(&ctx.label, None),
            _ => Ok(()),   // a refresh with nothing to change wasn't reviewed again: its points stand
        }
    }

    /// The step's settings with the names in its paths and commands filled in.
    pub fn rendered(&self, ctx: &Ctx, mut s: StepSettings) -> Result<StepSettings, Fatal> {
        let all = |v: Option<Vec<String>>| v.map(|v| v.iter().map(|x| self.render(ctx, x)).collect::<Result<Vec<_>, _>>()).transpose();
        s.inputs = all(s.inputs.take())?;
        s.outputs = all(s.outputs.take())?;
        s.note = s.note.take().map(|n| self.render(ctx, &n)).transpose()?;
        Ok(s)
    }

    /// The worker's prompt file: the step's own, or `<dirs.prompts>/<id>.md`, in the process home.
    fn prompt_path(&self, ctx: &Ctx) -> PathBuf {
        let rel = ctx.step.prompt.clone().unwrap_or_else(|| format!("{}/{}.md", self.shared.spec.dirs.prompts, ctx.id()));
        self.home.join(rel)
    }

    /// The critic's rubric file: the step's `review`, or `<dirs.reviews>/<id>-review.md`; none without one.
    pub fn rubric(&self, ctx: &Ctx) -> Option<PathBuf> {
        match &ctx.step.review {
            Some(Review::File(f)) => Some(self.home.join(f)),
            Some(Review::On(true)) => Some(self.home.join(format!("{}/{}-review.md", self.shared.spec.dirs.reviews, ctx.id()))),
            _ => None,
        }
    }

    /// The globs a `deny` list stands for: a name of the process's `deny:` lists is its entries, anything else a glob.
    pub fn deny_globs(&self, ctx: &Ctx, entries: &[String]) -> Result<Vec<String>, Fatal> {
        let mut out = vec![];
        for e in entries {
            match self.shared.spec.deny.get(e) {
                Some(list) => {
                    for g in list {
                        out.push(self.render(ctx, g)?);
                    }
                }
                None => out.push(self.render(ctx, e)?),
            }
        }
        Ok(out)
    }

    /// A check as a judge, through the step's names; none for one that only the last item of an `each` has.
    fn check_judge(&self, ctx: &Ctx, check: &Check) -> Result<Option<Judge>, Fatal> {
        if check.last_only() && !ctx.item.as_ref().is_some_and(|i| i.last) {
            return Ok(None);
        }
        self.judge(ctx, check.name(), check.command()).map(Some)
    }

    /// A section of a brief as text: its own, a file's (rendered) or a command's output.
    pub fn section_text(&self, ctx: &Ctx, s: &Section) -> Result<String, Fatal> {
        if let Some(t) = &s.text {
            return self.render(ctx, t);
        }
        if let Some(f) = &s.file {
            let text = std::fs::read_to_string(self.home.join(f)).map_err(|e| anyhow!("{f}: {e}"))?;
            return self.render(ctx, &text);
        }
        match &s.run {
            Some(c) => self.run(ctx, c),
            None => Ok(String::new()),
        }
    }

    /// The step as agg runs it: its brief's standing sections, its script first, and what it reads.
    fn build_task(&self, ctx: &Ctx, step: &Step, s: &StepSettings) -> Result<Task, Fatal> {
        let brief = &self.shared.spec.run.brief;
        let mut task = Task::new(step.clone()).named(&ctx.label);
        if let Some(spec) = &brief.inputs {
            task = task.section(inputs::section(&self.project, spec, &s.inputs.clone().unwrap_or_default()));
        }
        for section in &brief.sections {
            task = task.section(self.section_text(ctx, section)?);
        }
        if let Some(note) = s.note.as_deref().filter(|n| !n.trim().is_empty()) {
            task = task.section(note_section(note));
            if self.reaches(ctx) {
                self.record(&ctx.label, "note", true, &format!("the project's settings, step {}: {note}", ctx.id()));
            }
        }
        if let Some(command) = &s.first {
            let judge = self.judge(ctx, Some(&format!("{}-first", ctx.key)), command)?;
            task = task.first(self.cached(&judge)?);
        }
        if let Some(inputs) = &s.inputs {
            task = task.inputs(inputs.clone());
        }
        Ok(task)
    }

    /// What step `ctx`'s critic reviews: its scope, and when the step runs again over an accepted result (a note, a
    /// decision, a refresh), only what this run changes in it.
    fn critic_brief(&self, ctx: &Ctx, s: &StepSettings) -> Result<String, Fatal> {
        let outputs = s.outputs.clone().unwrap_or_default();
        let scope = self.scope_text(ctx, &outputs)?;
        let last = self.steps().last_commit(&ctx.label, &["done", "unchanged"]);
        Ok(match rerun::reason(&self.run, &ctx.key, last) {
            Some(why) => format!("{scope}\n\n{}", rerun::context(&why, &ctx.key, &outputs, &s.inputs.clone().unwrap_or_default())),
            None => scope,
        })
    }

    /// What a critic reviews and what it never raises (`run.critic_scope`, with `{{ outputs }}` filled in).
    fn scope_text(&self, ctx: &Ctx, outputs: &[String]) -> Result<String, Fatal> {
        let Some(file) = &self.shared.spec.run.critic_scope else { return Ok(String::new()) };
        let text = std::fs::read_to_string(self.home.join(file)).map_err(|e| anyhow!("{file}: {e}"))?;
        let what = if outputs.is_empty() { "the files this step wrote".to_string() }
                   else { outputs.iter().map(|o| format!("`{o}`")).collect::<Vec<_>>().join(", ") };
        self.render(ctx, &text.replace("{{ outputs }}", &what))
    }

    /// The code-quality floor of what step `ctx` changes (`quality: true`, with the floor set in the settings): the
    /// score of the files changed since the step's base, with the build's reports, through agg's score judge, and the
    /// worker's guideline to it; none when no floor is set.
    fn quality(&self, ctx: &Ctx) -> Result<Option<(Judge, String)>, Fatal> {
        let q = self.settings.quality();
        let spec = &self.shared.spec.settings.quality;
        let (true, Some(definition), Some(version), Some(min)) = (ctx.step.quality, q.definition, q.version, q.min) else {
            return Ok(None);
        };
        let since = bases::base(&ctx.key);
        let reports = spec.reports.as_ref().map(|r| self.home.join(r).display().to_string()).unwrap_or_default();
        let guide = match &spec.guide {
            Some(f) => std::fs::read_to_string(self.home.join(f)).map_err(|e| anyhow!("{f}: {e}"))?
                .replace("{min}", &min.to_string()).replace("{definition}", &definition).replace("{version}", &version)
                .replace("{since}", &since).replace("{reports}", &reports),
            None => String::new(),
        };
        let scope = self.render(ctx, spec.scope.as_deref().unwrap_or("."))?;
        let mut score = ScoreJudge::new(&definition, &version).min(min).scope(Scope::ChangedIn { path: scope, since });
        if !reports.is_empty() {
            score = score.reports(reports);
        }
        Ok(Some((Judge::score(format!("quality-{}", ctx.key), score), guide)))
    }
}

/// The section of a step's brief with the project's note for it: it holds for every run of the step, and is no goal:
/// it bounds how the step works, not what the app must do.
fn note_section(note: &str) -> String {
    format!("## The project's note for this step\n\nThe project sets this for every run of this step. It bounds how \
             you do the step; it is no goal of the app, so don't write it into a result as one.\n\n{note}")
}
