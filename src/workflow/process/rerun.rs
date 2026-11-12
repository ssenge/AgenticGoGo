//! What the critic of a step that runs again reviews (A-37): only what this run changes in a result that
//! was accepted before. A step re-entered with a note or a decision, or refreshed because what it reads changed,
//! gets it; a first run and a full redo (`--reset`, or `--from` without a note, after the step's own instructions
//! changed) get the full review.

use crate::workflow::{before, Run};

/// Why a step runs again over its accepted result.
#[derive(Debug, PartialEq)]
pub enum Rerun {
    /// Re-entered at it with a later step's note.
    Fix(String),
    /// Re-entered at it with what the humans decided at a checkpoint.
    Decided(String),
    /// Refreshed: what it reads changed since its accepted result, which stands on this commit.
    Refresh(String),
}

/// Why step `step` (its key) runs again in `run`, given the commit its last accepted result stands on (none: it
/// has no result to keep, so its review is the full one).
pub fn reason(run: &Run, step: &str, last: Option<String>) -> Option<Rerun> {
    let last = last.filter(|_| !run.reset && run.from.is_some())?;
    if run.from.as_deref() != Some(step) {
        return Some(Rerun::Refresh(last));
    }
    let note = run.note.clone()?;
    Some(if run.decided { Rerun::Decided(note) } else { Rerun::Fix(note) })
}

/// The critic's brief for a step (its key, what it writes and what it reads) that runs again for `why`.
pub fn context(why: &Rerun, step: &str, outputs: &[String], inputs: &[String]) -> String {
    let (title, asked, said) = match why {
        Rerun::Fix(note) => ("fixes", "A later step found the problem below; this run fixes only that".to_string(),
                             note.as_str()),
        Rerun::Decided(note) => ("carries a decision into", "At a checkpoint the humans decided what follows; this \
                                  run carries only that into the result".to_string(), note.as_str()),
        Rerun::Refresh(commit) => ("adjusts", format!("Since then, what it reads changed (`git diff {commit}{}`); \
                                   this run adjusts the result only where those changes require it", paths(inputs)), ""),
    };
    let tail = if said.is_empty() { String::new() } else { format!("\n\n{said}") };
    format!("## This run {title} an accepted result\n\nThis step's result was reviewed and accepted before. {asked}. \
             Review only that: point out where the result doesn't do it yet, and anything this run changed beyond it \
             (`git diff {}{}` shows this run's change). What this run didn't touch was reviewed and accepted before: \
             a point about it is no point, however true.{tail}", before(step), paths(outputs))
}

/// Paths as the end of a `git diff` command: none, or ` -- <path> …`.
fn paths(list: &[String]) -> String {
    if list.is_empty() { String::new() } else { format!(" -- {}", list.join(" ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(from: Option<&str>, note: Option<&str>, reset: bool, decided: bool) -> Run {
        Run { from: from.map(String::from), note: note.map(String::from), reset, decided, ..Run::default() }
    }

    /// The step re-entered with a note fixes, with a decision carries it; a later step refreshes.
    #[test]
    fn a_step_that_runs_again_over_its_result_says_why() {
        let last = || Some("abc".to_string());
        assert_eq!(reason(&run(Some("record"), Some("n"), false, false), "record", last()), Some(Rerun::Fix("n".into())));
        assert_eq!(reason(&run(Some("design"), Some("d"), false, true), "design", last()), Some(Rerun::Decided("d".into())));
        assert_eq!(reason(&run(Some("record"), Some("n"), false, false), "fit", last()), Some(Rerun::Refresh("abc".into())));
    }

    /// A first run, a result never accepted, a reset and a `--from` without a note are reviewed in full.
    #[test]
    fn a_first_run_a_reset_and_a_redo_get_the_full_review() {
        let last = || Some("abc".to_string());
        assert_eq!(reason(&run(None, None, false, false), "record", last()), None);
        assert_eq!(reason(&run(Some("record"), Some("n"), false, false), "record", None), None);
        assert_eq!(reason(&run(Some("record"), Some("n"), true, false), "fit", last()), None);
        assert_eq!(reason(&run(Some("record"), None, false, false), "record", last()), None);
    }

    /// The brief names the change to review, the note it fixes, and for a refresh the inputs' changes.
    #[test]
    fn the_critic_is_told_to_review_only_the_change() {
        let fix = context(&Rerun::Fix("two reads".into()), "record", &["oracles/capture.yaml".into()], &[]);
        assert!(fix.starts_with("## This run fixes an accepted result"));
        assert!(fix.contains("`git diff refs/agg/before/record -- oracles/capture.yaml`"));
        assert!(fix.ends_with("\n\ntwo reads"));
        let refresh = context(&Rerun::Refresh("abc".into()), "fit", &[], &["oracles/".into()]);
        assert!(refresh.contains("`git diff abc -- oracles/`") && refresh.contains("`git diff refs/agg/before/fit`"));
        assert!(context(&Rerun::Decided("d".into()), "design", &[], &[]).starts_with("## This run carries a decision"));
    }
}
