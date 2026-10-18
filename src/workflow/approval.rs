//! Approval steps (A-28): humans approve a document and write their own decisions into a file no step writes,
//! so their words survive when a later re-entry regenerates the document. An approval whose inputs didn't
//! change since it was given isn't asked again.

use super::flow::Flow;
use super::git::{commit, git, unchanged_since};
use crate::driver::Fatal;

impl Flow {
    /// Asks `question` about `document`; the humans first write any decision of theirs into `decisions` (a
    /// file the driver denies every step's session). Yes commits what they changed with the approval; no ends
    /// the run. After a re-entry, an approval none of whose `inputs` (none: the document) changed since it
    /// was given stands without asking.
    pub fn approval(&self, name: &str, document: &str, question: &str, decisions: &str, inputs: &[String])
                    -> Result<(), Fatal> {
        if !self.runs(name)? {
            return Ok(());
        }
        let watched = if inputs.is_empty() { vec![document.to_string()] } else { inputs.to_vec() };
        let reentered = self.run.from.is_some() && !self.run.reset;
        let given = self.steps().last_commit(name, &["approval"]).filter(|_| reentered);
        if given.is_some_and(|at| unchanged_since(self.project(), &at, &watched)) {
            self.record(name, "approval", true, &format!("unchanged since its approval: {}", watched.join(", ")));
            return self.flush();
        }
        let question = format!("{question} First write any decision of yours, or what to change, into {decisions}.");
        if !self.hil_bool(&question)? {
            return self.halt(format!("{name}: not approved"));
        }
        let message = format!("{name} approved");
        if !commit(self.project(), &["."], &message)? {
            git(self.project(), &["commit", "-q", "--allow-empty", "-m", &message])?;
        }
        self.record(name, "approval", true, &question);
        self.flush()
    }
}
