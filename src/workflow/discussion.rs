//! A discussion before an agent step (A-36): agents on other roles take positions on the step's task before
//! anyone carries it out, each on its own first, then in turn each reads the others' latest and revises its own,
//! until all agree or the rounds are spent. Agreement, not stillness, ends it: a diligent voice keeps refining
//! its points after the disagreements are gone, and the refining is the worker's. The worker then carries out the task with their last positions in its
//! brief. Not a critic: the voices shape the result before it exists, instead of judging it after.

use std::path::{Path, PathBuf};

use super::brief::{with_lead, with_section};
use super::flow::Flow;
use super::key;
use super::roles::Role;
use super::task::Task;
use crate::driver::{Fatal, Step};

/// Rounds in which the voices revise their positions: where they still disagree after them, the worker decides.
const ROUNDS: usize = 3;

impl Flow {
    /// `step` with the discussion of its task by `t`'s voices in its brief; without voices, `step` as it is.
    /// The voices read the task as the worker does, with its sections, but not how it is reviewed.
    pub(crate) fn discussed(&self, name: &str, step: &Step, t: &Task) -> Result<Step, Fatal> {
        let voices = &t.voices;
        if voices.is_empty() {
            return Ok(step.clone());
        }
        let topic = t.sections.iter().try_fold(step.clone(), |s, section| with_section(&s, section))?;
        let mut latest = vec![];
        let mut agreed = false;
        for round in 0..=ROUNDS {
            self.round(name, &topic, voices, round, &mut latest)?;
            agreed = round > 0 && latest.iter().all(|p| agrees(p));
            if agreed {
                break;
            }
        }
        let how = if agreed { "until they agreed".to_string() }
                  else { format!("for {ROUNDS} rounds, and still disagree on some points") };
        self.record(name, "discussion", agreed, &format!("{} voices discussed it {how}", voices.len()));
        with_section(step, &format!(
            "## The discussion before you\n\n{} agents discussed this task before you, {how}. Their last \
             positions are in {}. Read them first, and carry out the task as they agree. Where they disagree, \
             weigh their evidence and decide; name each such point in your result, with your decision and why.",
            voices.len(), listed(&latest)))
    }

    /// One round: each voice in turn writes its position into `latest`, on its own when it has none yet, else
    /// after reading the others' latest: a voice answers the ones before it in this round, not a round late.
    fn round(&self, name: &str, step: &Step, voices: &[Role], round: usize, latest: &mut Vec<PathBuf>)
             -> Result<(), Fatal> {
        for (i, voice) in voices.iter().enumerate() {
            let file = self.scratch(&format!("{}-position-{}-{round}.md", key(name), i + 1))?;
            let lead = match latest.get(i) {
                None => blind(voices.len(), i + 1, &file),
                Some(own) => {
                    let others: Vec<PathBuf> = latest.iter().enumerate().filter(|(j, _)| *j != i)
                        .map(|(_, p)| p.clone()).collect();
                    revised(i + 1, &file, own, &others)
                }
            };
            self.step(&as_voice(&with_lead(step, &lead)?, voice, format!("{name}: voice {} round {round}", i + 1)))?;
            if !file.exists() {
                self.fail(name, &format!("voice {} wrote no position in round {round}", i + 1))?;
            }
            if i < latest.len() { latest[i] = file } else { latest.push(file) }
        }
        Ok(())
    }
}

/// The brief's lead for voice `me`'s first position, taken without the others'.
fn blind(voices: usize, me: usize, file: &Path) -> String {
    format!("## Discuss this task; don't carry it out\n\nBefore anyone carries out the task below, {voices} agents \
             discuss it, each on its own first; you are voice {me}. Read what you need and change no file of the \
             project. Write your position to `{}`, starting with the line `# Position of voice {me}`: numbered \
             points, most important first, each with its evidence (a file and a short quote) and what to do; then \
             what you are unsure about.", file.display())
}

/// The brief's lead for voice `me`'s revised position: its own last one and the others'. A voice that knows which
/// position is its own doesn't argue its own points as another's.
fn revised(me: usize, file: &Path, own: &Path, others: &[PathBuf]) -> String {
    format!("## Discuss this task; don't carry it out\n\nBefore anyone carries out the task below, {} agents \
             discuss it; you are voice {me}. You wrote `{}`; the other voices wrote {}. Read them and change no \
             file of the project. For each of their points, say whether you agree, and if not, why, with \
             evidence; for each of yours, keep, change or drop it, with a reason. Change a point for evidence, \
             not to meet the others halfway. Write your position to `{}` in the same form, starting with the line \
             `# Position of voice {me}`, and end it with the line `DISAGREE: <n>`, where n is the number of the \
             other voices' points you still disagree with.", others.len() + 1, own.display(), listed(others),
            file.display())
}

/// The task's step on `voice`'s agent, model and effort, named `name`.
fn as_voice(step: &Step, voice: &Role, name: String) -> Step {
    let mut session = step.clone().agent(voice.agent.clone()).effort(voice.effort);
    session.model = voice.model.clone();
    session.name = Some(name);
    session
}

/// Whether the position in `file` ends with `DISAGREE: 0` (in any markup): its voice disputes none of the others'.
fn agrees(file: &Path) -> bool {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().replace(['*', '`', '_'], "");
    last.trim().to_ascii_lowercase().strip_prefix("disagree:").is_some_and(|n| n.trim() == "0")
}

/// `files` as a list for a brief.
fn listed(files: &[PathBuf]) -> String {
    files.iter().map(|f| format!("`{}`", f.display())).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A voice agrees only when its position's last line counts no disagreement, in whatever markup; a missing
    /// position agrees with nothing.
    #[test]
    fn a_voice_agrees_when_its_last_line_counts_no_disagreement() {
        let dir = tempfile::tempdir().unwrap();
        let said = |text: &str| {
            let file = dir.path().join("p.md");
            std::fs::write(&file, text).unwrap();
            agrees(&file)
        };
        assert!(said("1. a point\n\nDISAGREE: 0\n\n"));
        assert!(said("1. a point\n**Disagree: 0**"));
        assert!(!said("1. a point\nDISAGREE: 2\n"));
        assert!(!said("1. a point\nDISAGREE: 10\n"));
        assert!(!said("DISAGREE: 0\n1. a point added after it\n"));
        assert!(!agrees(&dir.path().join("missing.md")));
    }
}
