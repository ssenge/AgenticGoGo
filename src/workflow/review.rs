//! The judging side of an agent step: the critic's rubric in the worker's brief, the critic with the worker's
//! task below its rubric, the narrowed re-review after a repair, and the checks the worker runs itself.

use std::path::Path;

use anyhow::anyhow;

use super::brief::{task, with_section};
use super::flow::Flow;
use crate::core::model::{Judge, JudgeKind};
use crate::driver::{Fatal, Step};

/// What the last review found, and what the worker said against it, for the next round.
pub(crate) struct Earlier {
    pub(crate) round: usize,
    pub(crate) points: String,
    pub(crate) disputes: String,
    pub(crate) by_critic: bool,
}

/// The step with the critic's criteria in hand, so its first result aims at what the review checks.
pub(crate) fn with_rubric(step: &Step, critic: &Judge) -> Result<Step, Fatal> {
    let JudgeKind::Llm { path, .. } = &critic.kind else { return Ok(step.clone()) };
    let rubric = read(path)?;
    let body = match rubric.strip_prefix("---") {
        Some(rest) => rest.split_once("\n---").map_or(rest, |(_, after)| after),
        None => rubric.as_str(),
    };
    with_section(step, &format!(
        "## How your result is reviewed\n\nWhen you finish, a reviewer checks your result with the criteria \
         below. Meet them in your first result: before you finish, go through them one by one against your \
         result as the reviewer will, and fix what doesn't hold; each point the reviewer finds costs a whole \
         repair round.\n\n> {}", body.trim().replace('\n', "\n> ")))
}

/// The critic with `context` and the worker's task below its rubric, so the rubric judges against the task's
/// policy instead of restating it (a restated policy drifts from the task).
pub(crate) fn with_task(flow: &Flow, critic: &Judge, context: &str, step: &Step) -> Result<Judge, Fatal> {
    rewritten(flow, critic, &format!("{}-rubric.md", critic.name), |rubric| Ok(format!(
        "{rubric}\n\n{context}\n\n## The worker's task\n\nWhat the worker was asked, with the policy it follows. \
         Judge the result against it; it is context, not a list of points.\n\n{}\n", task(step)?)))
}

/// The critic's re-review: its rubric narrowed to its earlier points and the worker's disputes.
pub(crate) fn focused(flow: &Flow, critic: &Judge, e: &Earlier) -> Result<Judge, Fatal> {
    let disputes = if e.disputes.trim().is_empty() { "None." } else { e.disputes.trim() };
    rewritten(flow, critic, &format!("{}-rereview-{}.md", critic.name, e.round), |rubric| Ok(format!(
        "{rubric}\n\n## This is a re-review\n\nYou reviewed this result before; the worker has repaired it. \
         Check only whether each of your earlier points is fixed and whether the fixes broke something else; \
         don't review the rest again. For a point the worker disputes, weigh its reason: if it holds, drop \
         the point; if you still see the issue, repeat the point and say why the reason doesn't hold. Met \
         when every point that remains is fixed.\n\n### Your earlier points\n\n{}\n\n\
         ### The worker's disputes\n\n{disputes}\n", e.points)))
}

/// The critic with its rubric rewritten by `f` into the scratch file `file`.
fn rewritten(flow: &Flow, critic: &Judge, file: &str, f: impl FnOnce(&str) -> Result<String, Fatal>)
             -> Result<Judge, Fatal> {
    let mut judge = critic.clone();
    let JudgeKind::Llm { path, .. } = &mut judge.kind else { return Ok(judge) };
    let text = f(&read(path)?)?;
    let out = flow.scratch(file)?;
    std::fs::write(&out, text).map_err(|e| anyhow!("writing {}: {e}", out.display()))?;
    *path = out;
    Ok(judge)
}

/// The step with its checks in hand: the worker runs them before it finishes and fixes what they report,
/// in seconds, instead of a whole repair round after the session.
pub(crate) fn self_checked(step: &Step, checks: &[Judge]) -> Result<Step, Fatal> {
    let scripts: Vec<String> = checks.iter().filter_map(|c| match &c.kind {
        JudgeKind::Script { path } => Some(format!("`{}`", path.display())),
        _ => None,
    }).collect();
    if scripts.is_empty() {
        return Ok(step.clone());
    }
    with_section(step, &format!(
        "## Check your result\n\nBefore you finish, run {} in your working directory. Each prints a verdict as \
         JSON; while `met` is false, fix what its `rationale` says and run it again. The same checks judge your \
         result afterwards. A rationale says what is missing: act on it, and never read the checks' code to \
         find out how they work.", scripts.join(", ")))
}

/// The brief's way out for a failure the worker can't fix: its cause lies in what the step may not change.
pub(crate) fn cannot_fix(path: &Path) -> String {
    format!("## When you can't fix it\n\nIf a check fails because of something outside what you may change (a \
             file another step owns, a decision, the environment), don't try around it: write in two or three \
             sentences what fails, why you can't fix it, and which step or decision can, to {}, and finish.",
            path.display())
}

/// Why another repair round is pointless, if it is: the worker said it can't fix the failure, or a check
/// (not the critic, whose repeated points a human weighs) failed exactly as in the round before.
pub(crate) fn stuck(cannot: &Path, rationale: &str, by_critic: bool, previous: Option<&str>) -> Option<String> {
    let said = std::fs::read_to_string(cannot).unwrap_or_default();
    if !said.trim().is_empty() {
        return Some(format!("the worker can't fix it: {} ({rationale})", said.trim()));
    }
    (!by_critic && previous == Some(rationale)).then(|| format!("the same failure after a repair: {rationale}"))
}

fn read(path: &Path) -> Result<String, Fatal> {
    Ok(std::fs::read_to_string(path).map_err(|e| anyhow!("reading {}: {e}", path.display()))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repair round is pointless when the worker says it can't fix the failure, or a check fails alike again.
    #[test]
    fn a_repair_round_is_skipped_when_the_worker_cant_fix_it_or_a_check_fails_the_same_again() {
        let dir = tempfile::tempdir().unwrap();
        let cannot = dir.path().join("cannot-fix.md");
        assert_eq!(stuck(&cannot, "gate x", false, None), None);
        assert!(stuck(&cannot, "gate x", false, Some("gate x")).unwrap().starts_with("the same failure"));
        assert_eq!(stuck(&cannot, "gate x", true, Some("gate x")), None, "a human weighs the critic's points");
        std::fs::write(&cannot, "the measure is 2.9's").unwrap();
        assert!(stuck(&cannot, "gate x", true, None).unwrap().contains("can't fix it: the measure is 2.9's"));
    }

    /// The worker sees the rubric without its frontmatter, and the script checks it must run.
    #[test]
    fn the_brief_holds_the_rubric_and_the_checks_to_run() {
        let dir = tempfile::tempdir().unwrap();
        let rubric = dir.path().join("review.md");
        std::fs::write(&rubric, "---\ninputs: [a]\n---\nIs it right?\n").unwrap();
        let step = Step::new("1 Do").prompt("Do it.");
        let critic = Judge::rubric("review", rubric.display().to_string());
        let briefed = with_rubric(&step, &critic).unwrap().prompt.unwrap();
        assert!(briefed.contains("> Is it right?") && !briefed.contains("inputs: [a]"));
        let checked = self_checked(&step, &[Judge::script("c", "/x/check.sh"), critic]).unwrap().prompt.unwrap();
        assert!(checked.contains("run `/x/check.sh` in your working directory"));
        assert_eq!(self_checked(&step, &[]).unwrap().prompt.as_deref(), Some("Do it."));
    }
}
