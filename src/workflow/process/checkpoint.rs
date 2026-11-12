//! A checkpoint (A-37): humans approve, in one answer, the document that sums up
//! what the steps before it produced, with the reviewers' open points and the task force's repairs in place since the
//! last one; their own words go into the checkpoint's decisions file, which no step writes.

use std::collections::HashSet;
use std::path::Path;

use anyhow::anyhow;

use super::exec::Ctx;
use super::proc::Proc;
use crate::driver::Fatal;
use crate::workflow::commit;

impl Proc {
    /// The checkpoint of `ctx`: its document is prepared (a script step), then asked about.
    pub fn checkpoint(&self, ctx: &Ctx) -> Result<(), Fatal> {
        let s = ctx.step;
        let (doc, question) = (s.doc.as_deref().unwrap_or_default(), s.question.as_deref().unwrap_or_default());
        // the document is written by `prepare`, a step of its own; without it an earlier step wrote it
        if let Some(prepare) = &s.prepare {
            let prepared = format!("{} document", ctx.label);
            let key = format!("{}-document", ctx.key);
            self.register(&prepared, &key, &key, &ctx.num.phase_id);
            let judge = self.judge(ctx, Some(&key), prepare)?;
            self.script_step(&prepared, &judge)?;
        }
        // one the run skips weighs nothing: its open points wait for the next checkpoint that asks
        if !self.reaches(ctx) {
            return self.runs(&ctx.label).map(drop);
        }
        let doc = self.render(ctx, doc)?;
        let question = self.render(ctx, question)?;
        let cp = ctx.num.cp.clone().unwrap_or_default();
        if let Some(points) = self.open_points_file() {
            for (step, what) in repaired_since_approval(&self.steps().entries()) {
                append(&self.project.join(&points), &format!("- **{step}, repaired outside the process order:** {what}"))?;
            }
        }
        if self.settings.auto_approved(&cp, ctx.id(), s.go_live) {
            return self.auto_approve(ctx, &doc);
        }
        let open = self.open_points_file().map(|f| std::fs::read_to_string(self.project.join(f)).unwrap_or_default()).unwrap_or_default();
        let question = match self.open_points_file() {
            Some(f) if !open.trim().is_empty() => format!("{question} Reviewers left open points, kept for you to weigh: read {f}."),
            _ => question,
        };
        let mut inputs = self.settings.for_step(&ctx.key, ctx.id()).inputs.unwrap_or_default();
        // new points since its approval are something to weigh: a re-entered checkpoint asks again for them
        if let (Some(f), false) = (self.open_points_file(), open.trim().is_empty()) {
            inputs.push(f);
        }
        let dir = Path::new(&doc).parent().map(|p| p.display().to_string()).unwrap_or_default();
        let decisions = if dir.is_empty() { format!("{}-decisions.md", ctx.id()) } else { format!("{dir}/{}-decisions.md", ctx.id()) };
        let before = std::fs::read_to_string(self.project.join(&decisions)).unwrap_or_default();
        self.approval(&ctx.label, &doc, &question, &decisions, &inputs)?;
        if let (Some(f), false) = (self.open_points_file(), open.trim().is_empty()) {
            // weighed: git keeps them in the history
            std::fs::write(self.project.join(&f), "").map_err(|e| anyhow!("clearing {f}: {e}"))?;
            commit(&self.project, &[&f], &format!("{}: open points weighed", ctx.label))?;
        }
        // a decision taken in the yes: the step that wrote the document carries it before anything builds on it
        let added = added_lines(&before, &std::fs::read_to_string(self.project.join(&decisions)).unwrap_or_default());
        match &s.owner {
            Some(owner) if !added.is_empty() => self.reenter(owner, &format!("{} decided ({decisions}):\n\n{added}", ctx.label)),
            _ => Ok(()),
        }
    }

    /// A checkpoint the project approves itself in a test run: the run goes on; its document and open points wait for
    /// the humans. Its own kind of record, so a later run without the setting asks them.
    fn auto_approve(&self, ctx: &Ctx, doc: &str) -> Result<(), Fatal> {
        if !self.runs(&ctx.label)? {
            return Ok(());
        }
        commit(&self.project, &["."], &format!("{} auto-approved", ctx.label))?;
        let waiting = match &self.shared.spec.run.auto_note {
            Some(c) => self.run(ctx, c).unwrap_or_default().trim().to_string(),
            None => String::new(),
        };
        let waiting = if waiting.is_empty() { String::new() } else { format!("; waiting for them: {waiting}") };
        self.info(&format!("{}: auto-approved (the project's `auto_approve`); {doc} waits for the humans{waiting}", ctx.label));
        self.record(&ctx.label, "auto-approval", true, &format!("approved by the project's `auto_approve`, {doc} not read by a human{waiting}"));
        self.flush()
    }

    /// The file the reviewers' open points are kept in until the next checkpoint, if the process has one.
    fn open_points_file(&self) -> Option<String> {
        self.shared.spec.run.open_points.clone()
    }

    /// The critic's point after its repair round goes to the next checkpoint's humans, in place of the step's earlier
    /// ones; a step whose critic is met leaves none.
    pub fn open_point(&self, label: &str, point: Option<&str>) -> Result<(), Fatal> {
        let Some(file) = self.open_points_file() else { return Ok(()) };
        let path = self.project.join(&file);
        let before = std::fs::read_to_string(&path).unwrap_or_default();
        let after = replaced(&before, label, point);
        if after == before {
            return Ok(());
        }
        write(&path, &after)?;
        let what = if point.is_some() { "open point for the next checkpoint" } else { "its open points resolved" };
        commit(&self.project, &[&file], &format!("{label}: {what}"))?;
        Ok(())
    }
}

/// What the task force repaired in place since the last approval: (step, what it changed and why), oldest first.
fn repaired_since_approval(entries: &[serde_json::Value]) -> Vec<(String, String)> {
    let start = entries.iter().rposition(|e| e["kind"] == "approval" || e["kind"] == "auto-approval").map_or(0, |i| i + 1);
    entries[start..].iter()
        .filter(|e| e["kind"] == "task-force" && e["met"] == true)
        .filter_map(|e| Some((e["step"].as_str()?.to_string(), e["rationale"].as_str()?.strip_prefix("fixed in place ")?.to_string())))
        .collect()
}

/// The lines of `after` that `before` doesn't have: what the humans added to a decisions file.
fn added_lines(before: &str, after: &str) -> String {
    let had: HashSet<&str> = before.lines().collect();
    after.lines().filter(|l| !l.trim().is_empty() && !had.contains(l)).collect::<Vec<_>>().join("\n")
}

/// The open points `text` with step `name`'s own replaced by `point`, if any.
fn replaced(text: &str, name: &str, point: Option<&str>) -> String {
    let own = format!("- **{name}:**");
    let mut lines: Vec<String> = text.lines().filter(|l| !l.starts_with(&own)).map(String::from).collect();
    lines.extend(point.map(|p| format!("{own} {}", p.replace('\n', " "))));
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// Appends `line` to the file at `path`.
fn append(path: &Path, line: &str) -> Result<(), Fatal> {
    write(path, &format!("{}{line}\n", std::fs::read_to_string(path).unwrap_or_default()))
}

/// Writes `text` to the file at `path`, its folder made first: the first open point may come before any checkpoint
/// wrote one.
fn write(path: &Path, text: &str) -> Result<(), Fatal> {
    std::fs::create_dir_all(path.parent().unwrap_or(path)).and_then(|()| std::fs::write(path, text))
        .map_err(|e| anyhow!("writing {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decision added in a checkpoint's yes is what goes back to the document's step.
    #[test]
    fn a_decision_added_in_the_yes_is_what_goes_back() {
        assert_eq!(added_lines("- Q1: (a)\n", "- Q1: (a)\n\n- Q2: internal only\n"), "- Q2: internal only");
        assert_eq!(added_lines("- Q1: (a)\n", "- Q1: (a)\n"), "");
    }

    /// The repairs in place since the last approval are listed; a re-entry the task force proposed, or one before the
    /// approval, is not.
    #[test]
    fn the_repairs_in_place_since_the_last_approval_are_listed() {
        let e = |step: &str, kind: &str, met: bool, why: &str| serde_json::json!({"step": step, "kind": kind, "met": met, "rationale": why});
        let entries = [e("2.5", "task-force", true, "fixed in place (a): old"), e("3.7 CP2", "approval", true, "ok"),
                       e("4.4", "task-force", true, "fixed in place (bindings.lock.yaml): no tool: bound one"),
                       e("4.6", "task-force", true, "re-enter at 4.1: x"), e("4.6b", "task-force", false, "no re-entry: y")];
        assert_eq!(repaired_since_approval(&entries),
                   [("4.4".to_string(), "(bindings.lock.yaml): no tool: bound one".to_string())]);
    }

    /// A step's new point replaces its own earlier ones, none removes them, and another step's points stay.
    #[test]
    fn a_step_reviewed_again_replaces_its_own_open_points() {
        let text = "- **2.11a Legacy report:** keep\n- **3.2 Acceptance tests:** old\n- **3.2 Acceptance tests, repaired outside the process order:** x\n";
        assert_eq!(replaced(text, "3.2 Acceptance tests", None),
                   "- **2.11a Legacy report:** keep\n- **3.2 Acceptance tests, repaired outside the process order:** x\n");
        assert_eq!(replaced(text, "3.2 Acceptance tests", Some("new\nline")),
                   "- **2.11a Legacy report:** keep\n- **3.2 Acceptance tests, repaired outside the process order:** x\n- **3.2 Acceptance tests:** new line\n");
        assert_eq!(replaced("", "3.2 Acceptance tests", None), "");
    }

    /// An open point lands in its file whether or not its folder exists yet, after the points before it.
    #[test]
    fn an_open_point_makes_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("checkpoints/open-points.md");
        append(&file, "- **a:** one").unwrap();
        append(&file, "- **b:** two").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "- **a:** one\n- **b:** two\n");
    }
}
