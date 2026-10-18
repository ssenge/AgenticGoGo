//! The agents' own sessions per step (A-33 in a workflow): a step that ran before continues its own session (a
//! repair, a re-entry, a refresh: it knows what it wrote and why), and a step with a `context` step starts
//! from a fork of that step's session (it read the same inputs). Otherwise, and whenever the agent can't, the
//! step starts fresh: the session is a cache, the files are the record.

use std::collections::BTreeMap;
use std::path::Path;

use super::flow::Flow;
use super::key;
use crate::driver::{AgentSession, Fatal, Step, StepOutcome};

/// Where the sessions are kept: in the project's git folder, so they outlast a run and its re-entries and are
/// never committed.
const FILE: &str = ".git/agg-sessions.json";

/// Runs `step` as the flow's step `name` from its session, and remembers the session it ran in.
pub(crate) fn step(flow: &Flow, name: &str, step: &Step, context: Option<&str>) -> Result<StepOutcome, Fatal> {
    let mut known = load(flow.project());
    let outcome = flow.step(&started(step, known.get(key(name)), context.and_then(|c| known.get(c))))?;
    if let Some(session) = &outcome.agent_session {
        known.insert(key(name).to_string(), session.clone());
        let _ = std::fs::write(flow.project().join(FILE), serde_json::to_string(&known).unwrap_or_default());
    }
    Ok(outcome)
}

/// The step continuing its own session, else forking the context step's, else fresh.
fn started(step: &Step, own: Option<&AgentSession>, context: Option<&AgentSession>) -> Step {
    match (own, context) {
        (Some(own), _) => step.clone().resume(own),
        (None, Some(theirs)) => step.clone().fork(theirs),
        (None, None) => step.clone(),
    }
}

/// Forgets every step's session: a reset took their results back, so their sessions describe work that is gone.
pub fn forget(project: &Path) {
    let _ = std::fs::remove_file(project.join(FILE));
}

fn load(project: &Path) -> BTreeMap<String, AgentSession> {
    std::fs::read_to_string(project.join(FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step that ran before continues its own session; else it forks its context step's; else it starts fresh.
    #[test]
    fn a_step_continues_its_own_session_before_forking_its_context() {
        let (own, theirs) = (AgentSession { agent: "claude".into(), id: "a".into() },
                             AgentSession { agent: "claude".into(), id: "b".into() });
        let step = Step::new("8.2 Ops docs");
        assert_eq!(started(&step, Some(&own), Some(&theirs)).resume, Some((own.clone(), false)));
        assert_eq!(started(&step, None, Some(&theirs)).resume, Some((theirs, true)));
        assert_eq!(started(&step, None, None).resume, None);
    }

    /// The sessions outlast the run in the git folder, and a reset forgets them.
    #[test]
    fn sessions_are_kept_until_a_reset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(FILE), r#"{"8.1":{"agent":"claude","id":"a"}}"#).unwrap();
        assert_eq!(load(dir.path())["8.1"].id, "a");
        forget(dir.path());
        assert!(load(dir.path()).is_empty());
    }
}
