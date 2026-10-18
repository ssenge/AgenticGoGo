//! The step history (A-27): one JSON line per event of a workflow run — what each step did, its verdict and
//! the commit its result stands on — so a driver can report on a run and a re-entry can go back to a step or
//! refresh what came after it.

use std::cell::RefCell;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::anyhow;

use super::git::{git, git_out};

/// Where the history lives unless the driver names another file.
pub const DEFAULT: &str = "agg/private/steps.jsonl";

/// The history file and the entries kept back until the next step boundary: an entry written while a
/// session's work waits on its span would leave the project changed, and keeping the work would fail.
pub struct History {
    project: PathBuf,
    path: PathBuf,
    pending: RefCell<Vec<String>>,
}

impl History {
    /// The history of `project` in `path` (relative to the project, or absolute).
    pub fn new(project: &Path, path: impl AsRef<Path>) -> History {
        History { project: project.to_path_buf(), path: project.join(path), pending: RefCell::new(vec![]) }
    }

    /// Keeps one entry back until the next `flush`: `kind` is what happened (`done`, `tool`, `skipped`, …).
    pub fn defer(&self, step: &str, kind: &str, met: bool, rationale: &str) {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let commit = git_out(&self.project, &["rev-parse", "HEAD"]).unwrap_or_default();
        let line = serde_json::json!({"step": step, "kind": kind, "met": met, "rationale": rationale, "at": at,
                                      "commit": commit});
        self.pending.borrow_mut().push(line.to_string());
    }

    /// Writes the entries kept back.
    pub fn flush(&self) -> anyhow::Result<()> {
        let lines = std::mem::take(&mut *self.pending.borrow_mut());
        if lines.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(self.path.parent().unwrap_or(&self.project))
            .and_then(|_| std::fs::OpenOptions::new().create(true).append(true).open(&self.path))
            .and_then(|mut f| writeln!(f, "{}", lines.join("\n")))
            .map_err(|e| anyhow!("writing {}: {e}", self.path.display()))
    }

    /// Commits the history when git tracks its folder, so the history travels with the project.
    pub fn commit(&self) -> anyhow::Result<()> {
        let rel = self.path.strip_prefix(&self.project).unwrap_or(&self.path).display().to_string();
        let ignored = git(&self.project, &["check-ignore", "-q", &rel]).is_ok();
        if self.path.exists() && !ignored {
            super::git::commit(&self.project, &[&rel], "Step history")?;
        }
        Ok(())
    }

    /// Every entry, oldest first; none when the file is missing.
    pub fn entries(&self) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// The commit the last entry of step `name` with one of `kinds` stands on.
    pub fn last_commit(&self, name: &str, kinds: &[&str]) -> Option<String> {
        self.entries().into_iter().rev().find(|e| e["step"] == name && kinds.iter().any(|k| e["kind"] == *k))
            .and_then(|e| e["commit"].as_str().map(String::from)).filter(|c| !c.is_empty())
    }

    /// Whether the step with key `key` is an approval: what is decided there only humans decide.
    pub fn is_approval(&self, key: &str) -> bool {
        self.entries().iter()
            .any(|e| e["kind"] == "approval" && e["step"].as_str().map(super::key) == Some(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step's last result of the wanted kinds, and whether a key is an approval, come from the file.
    #[test]
    fn the_history_finds_a_steps_latest_result_and_its_approvals() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agg/private")).unwrap();
        std::fs::write(dir.path().join(DEFAULT), [
            r#"{"step": "4.1 Gate profile", "kind": "done", "commit": "aaa"}"#,
            r#"{"step": "4.1 Gate profile", "kind": "unchanged", "commit": "bbb"}"#,
            r#"{"step": "4.5 CP3", "kind": "approval", "commit": "ccc"}"#,
        ].join("\n")).unwrap();
        let h = History::new(dir.path(), DEFAULT);
        assert_eq!(h.last_commit("4.1 Gate profile", &["done", "unchanged"]).as_deref(), Some("bbb"));
        assert_eq!(h.last_commit("9.9 Nothing", &["done"]), None);
        assert!(h.is_approval("4.5") && !h.is_approval("4.1"));
    }

    /// Entries wait until a flush, then append.
    #[test]
    fn entries_are_kept_back_until_flushed() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path(), "steps.jsonl");
        h.defer("1 Fetch", "tool", true, "fetched");
        assert!(h.entries().is_empty());
        h.flush().unwrap();
        h.defer("2 Plan", "done", true, "planned");
        h.flush().unwrap();
        assert_eq!(h.entries().len(), 2);
        assert_eq!(h.entries()[1]["kind"], "done");
    }
}
