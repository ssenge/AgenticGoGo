//! The blocks every kind of step goes through (A-37): whether a step is switched off, whether it
//! applies, a script step, and what the run has come to of its `--from`.

use super::exec::Ctx;
use super::proc::Proc;
use crate::core::model::Judge;
use crate::driver::Fatal;

impl Proc {
    /// Whether the step of `ctx` is switched off (the settings' `skip`, or `switched_off` in the run's configuration
    /// file, by its id, its phase's or its `each`'s); one the run comes to is recorded as skipped, with why.
    pub fn off(&self, ctx: &Ctx) -> Result<bool, Fatal> {
        let mut names = vec![ctx.id().to_string(), ctx.num.phase_id.clone()];
        names.extend(ctx.num.parent.clone());
        let why = if names.iter().any(|n| self.settings.skipped(n)) {
            "switched off (`skip` in the process file; `skip: []` in the project's settings runs it)".to_string()
        } else if let Some((file, why)) = self.switched_off(&names) {
            format!("switched off by the run's configuration ({file}): {why}")
        } else {
            return Ok(false);
        };
        if self.runs(&ctx.label)? {
            self.info(&format!("{}: skipped, {why}", ctx.label));
            self.record(&ctx.label, "skipped", true, &why);
            self.flush()?;
        }
        Ok(true)
    }

    /// Why the run's configuration (`run.config`, written by a step) switches off one of `names`.
    fn switched_off(&self, names: &[String]) -> Option<(String, String)> {
        let file = self.shared.spec.run.config.as_ref()?;
        let text = std::fs::read_to_string(self.project.join(file)).unwrap_or_default();
        let run: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
        let off = run.get("switched_off")?;
        names.iter().find_map(|n| Some((file.clone(), off.get(n.as_str())?.as_str()?.to_string())))
    }

    /// Whether the run has come to the step of `ctx`: it starts there or got past its `--from` (agg marks every step it
    /// comes to as seen). A step before it is skipped, which says nothing about the step itself.
    pub fn reaches(&self, ctx: &Ctx) -> bool {
        reached(self.run.from.as_deref(), &ctx.key, &self.seen())
    }

    /// Ends the run halted when it never came to its `--from` step: a key no step has would end a run that did
    /// nothing as done.
    pub fn came_to_from(&self) -> Result<(), Fatal> {
        match self.run.from.as_deref() {
            Some(from) if !self.seen().iter().any(|s| s == from) => self.halt(format!(
                "--from {from}: the run came to no step {from} (an instance of an `each` is `<id>-<item>`, e.g. build-U1)")),
            _ => Ok(()),
        }
    }

    /// Whether the step applies to this app: every `when` command is met, the first unmet one decides; one that doesn't
    /// is skipped. One the run hasn't come to (before its `--from`) counts as applying: the steps it guards may be
    /// where the run starts.
    pub fn applies_to(&self, ctx: &Ctx) -> Result<bool, Fatal> {
        let Some(when) = &ctx.step.when else { return Ok(true) };
        if !self.reaches(ctx) {
            return self.runs(&ctx.label).map(|_| true);   // agg marks it seen and runs nothing
        }
        if !self.runs(&ctx.label)? {
            return Ok(false);
        }
        for (i, command) in when.all().into_iter().enumerate() {
            let judge = self.judge(ctx, Some(&format!("{}-when-{i}", ctx.key)), command)?;
            let verdict = self.condition(&judge);
            if let Some(error) = verdict.error() {
                self.fail(&ctx.label, &format!("whether it applies can't be told: {error}"))?;
                return Ok(false);
            }
            if !verdict.met() {
                self.info(&format!("{}: skipped, it doesn't apply: {}", ctx.label, verdict.rationale()));
                self.record(&ctx.label, "skipped", true, &format!("it doesn't apply: {}", verdict.rationale()));
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Runs the commands of a step's `pre` or `post`, each to its end: a failure fails the step.
    pub fn commands(&self, ctx: &Ctx, list: &[String]) -> Result<(), Fatal> {
        for c in list {
            if let Err(e) = self.run(ctx, c) {
                self.fail(&ctx.label, &e.to_string())?;
            }
        }
        Ok(())
    }

    /// A script step: its command does the work and gives the verdict. Met, what it wrote is committed and the step
    /// recorded (kind `tool`); unmet, the step fails, and after the task force's repair in place it runs again.
    pub fn script(&self, ctx: &Ctx, command: &str) -> Result<(), Fatal> {
        let judge: Judge = self.judge(ctx, Some(&ctx.key), command)?;
        self.script_step(&ctx.label, &judge)
    }
}

/// Whether a run from step `from` (none: from the start) has come to step `step`, having seen `seen`.
fn reached(from: Option<&str>, step: &str, seen: &[String]) -> bool {
    from.is_none_or(|f| f == step || seen.iter().any(|s| s == f))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run has come to a step with no `--from`, at its `--from` and after it; before it, not.
    #[test]
    fn a_run_comes_to_its_from_step_and_every_one_after_it() {
        let seen = ["cp-assessment".to_string(), "transform-U1".to_string()];
        assert!(reached(None, "cp-assessment", &[]));
        assert!(reached(Some("transform-U1"), "transform-U1", &seen[..1]));
        assert!(reached(Some("transform-U1"), "deploy", &seen));
        assert!(!reached(Some("transform-U1"), "cp-assessment", &seen[..1]));
        assert!(!reached(Some("transform"), "deploy", &seen));
    }
}
