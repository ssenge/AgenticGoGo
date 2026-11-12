//! The step history (A-27): one JSON line per event of a workflow run — what each step did, its verdict and
//! the commit its result stands on — so a driver can report on a run and a re-entry can go back to a step or
//! refresh what came after it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::anyhow;

use super::git::{git, git_out};

/// Where the history lives unless the driver names another file.
pub const DEFAULT: &str = "agg/private/steps.jsonl";

thread_local! {
    /// The history and the step running in it: an error that ends the run past every block (a driver's own,
    /// returned with `?`) is recorded against that step, since nothing else of the run sees it.
    static AT: RefCell<Option<(PathBuf, PathBuf, String, String)>> = const { RefCell::new(None) };
}

/// The history file and the entries kept back until the next step boundary: an entry written while a
/// session's work waits on its span would leave the project changed, and keeping the work would fail.
pub struct History {
    project: PathBuf,
    path: PathBuf,
    pending: RefCell<Vec<String>>,
    /// the keys of the steps this run recorded a result of
    given: RefCell<Vec<String>>,
    /// the key of each step a driver named explicitly (A-37); a step not named has its name's first word
    keys: RefCell<HashMap<String, String>>,
}

impl History {
    /// The history of `project` in `path` (relative to the project, or absolute).
    pub fn new(project: &Path, path: impl AsRef<Path>) -> History {
        History { project: project.to_path_buf(), path: project.join(path), pending: RefCell::new(vec![]),
                  given: RefCell::new(vec![]), keys: RefCell::new(HashMap::new()) }
    }

    /// Names the key of step `name` (a process file's id): what the history, the refs and `--from` go by.
    pub(crate) fn name_key(&self, name: &str, key: &str) {
        self.keys.borrow_mut().insert(name.to_string(), key.to_string());
    }

    /// The key of step `name`: the one named for it, else its first word.
    pub(crate) fn key_of(&self, name: &str) -> String {
        self.keys.borrow().get(name).cloned().unwrap_or_else(|| super::key(name).to_string())
    }

    /// Keeps one entry back until the next `flush`: `kind` is what happened (`done`, `tool`, `skipped`, …).
    pub fn defer(&self, step: &str, kind: &str, met: bool, rationale: &str) {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let commit = git_out(&self.project, &["rev-parse", "HEAD"]).unwrap_or_default();
        let key = self.key_of(step);
        let line = serde_json::json!({"step": step, "id": key, "kind": kind, "met": met, "rationale": rationale,
                                      "at": at, "commit": commit});
        self.pending.borrow_mut().push(line.to_string());
        if matches!(kind, "done" | "unchanged") {
            self.given.borrow_mut().push(key);
        }
    }

    /// Whether this run recorded a result (`done`, `unchanged`) of step `name`.
    pub(crate) fn gave(&self, name: &str) -> bool {
        let key = self.key_of(name);
        self.given.borrow().contains(&key)
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

    /// Marks step `name` as the one running, for [`ended_by`].
    pub(crate) fn at(&self, name: &str) {
        let key = self.key_of(name);
        AT.with(|at| *at.borrow_mut() = Some((self.project.clone(), self.path.clone(), name.to_string(), key)));
    }

    /// Every entry, oldest first; none when the file is missing.
    pub fn entries(&self) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// The commit the last entry of step `name` with one of `kinds` stands on.
    pub fn last_commit(&self, name: &str, kinds: &[&str]) -> Option<String> {
        let key = self.key_of(name);
        self.entries().into_iter().rev().find(|e| entry_key(e) == key && kinds.iter().any(|k| e["kind"] == *k))
            .and_then(|e| e["commit"].as_str().map(String::from)).filter(|c| !c.is_empty())
    }

    /// Whether the step with key `key` is an approval: what is decided there only humans decide.
    pub fn is_approval(&self, key: &str) -> bool {
        self.entries().iter().any(|e| e["kind"] == "approval" && entry_key(e) == key)
    }
}

/// The key of the step an entry is of: its `id`, else (an entry written before ids) its step's first word.
fn entry_key(entry: &serde_json::Value) -> String {
    entry["id"].as_str().map(String::from).unwrap_or_else(|| super::key(entry["step"].as_str().unwrap_or_default()).to_string())
}

/// Records `why`, the error that ended the run, as an `error` of the step running then; none ran, nothing.
pub(crate) fn ended_by(why: &str) {
    let Some((project, path, name, key)) = AT.with(|at| at.borrow_mut().take()) else { return };
    let history = History::new(&project, path);
    history.name_key(&name, &key);
    history.defer(&name, "error", false, why);
    // the run ends either way: a history it can't write is no reason to hide the error it ends with
    let _ = history.flush().and_then(|()| history.commit());
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

    /// A step named by a key keeps its history when its label changes (a number moved); an entry written
    /// before ids is matched by its first word.
    #[test]
    fn a_steps_history_follows_its_key_not_its_label() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path(), "steps.jsonl");
        h.name_key("3.4 As-is architecture", "as-is-architecture");
        h.name_key("3.5 CP1", "cp-assessment");
        h.defer("3.4 As-is architecture", "done", true, "ok");
        h.defer("3.5 CP1", "approval", true, "yes");
        h.flush().unwrap();
        assert_eq!(h.entries()[0]["id"], "as-is-architecture");
        assert!(h.is_approval("cp-assessment") && !h.is_approval("as-is-architecture"));
        // the same steps, one phase later in a rearranged process: the history is found by key
        let moved = History::new(dir.path(), "steps.jsonl");
        moved.name_key("4.4 As-is architecture", "as-is-architecture");
        assert_eq!(moved.key_of("4.4 As-is architecture"), "as-is-architecture");
        assert!(entry_key(&moved.entries()[0]) == moved.key_of("4.4 As-is architecture"));
        assert_eq!(moved.key_of("9.9 Unnamed step"), "9.9", "a step not named has its first word");
        let old = serde_json::json!({"step": "2.5 Bind", "kind": "approval"});
        assert_eq!(entry_key(&old), "2.5", "an entry without an id is matched by its first word");
    }

    /// The error that ends a run lands on the step running then, once; with no step running, nowhere.
    #[test]
    fn the_error_that_ends_a_run_is_recorded_against_its_step() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path(), "steps.jsonl");
        ended_by("before any step");
        h.at("3.2 Capture");
        ended_by("writing x: No such file or directory");
        ended_by("again");
        let entries = h.entries();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!((entries[0]["step"].as_str(), entries[0]["kind"].as_str()), (Some("3.2 Capture"), Some("error")));
        assert_eq!(entries[0]["rationale"], "writing x: No such file or directory");
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
