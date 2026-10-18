//! The git a workflow needs between its steps: committing what a script step wrote, the ref of where the base
//! branch stood before a step, what changed since a step's result, and taking later results back.

use std::path::Path;
use std::process::Command;

use anyhow::anyhow;

/// Runs git in `dir`; fails when git does.
pub(crate) fn git(dir: &Path, args: &[&str]) -> anyhow::Result<()> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if out.status.success() { Ok(()) } else { Err(anyhow!("git {} failed in {}", args.join(" "), dir.display())) }
}

/// Runs git in `dir` and returns its output; fails when git does.
pub(crate) fn git_out(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if out.status.success() { Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()) }
    else { Err(anyhow!("git {} failed in {}", args.join(" "), dir.display())) }
}

/// Commits what changed under `paths`; false when nothing did.
pub fn commit(dir: &Path, paths: &[&str], message: &str) -> anyhow::Result<bool> {
    git(dir, &[&["add", "-A", "--"], paths].concat())?;
    let staged = git(dir, &["diff", "--cached", "--quiet"]).is_err();
    if staged {
        git(dir, &["commit", "-q", "-m", message])?;
    }
    Ok(staged)
}

/// The ref that holds where the base branch stood before the step with key `key` ran.
pub fn before(key: &str) -> String {
    format!("refs/agg/before/{key}")
}

/// Whether nothing under `paths` changed between `commit` and HEAD.
pub fn unchanged_since(dir: &Path, commit: &str, paths: &[String]) -> bool {
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    git(dir, &[&["diff", "--quiet", commit, "HEAD", "--"], &paths[..]].concat()).is_ok()
}

/// What changed in the project since `commit`, as a stat and a diff cut at `limit` bytes; `skip` are
/// pathspecs left out (the run's own records).
pub fn changes_since(dir: &Path, commit: &str, skip: &[String], limit: usize) -> (String, String) {
    let skip: Vec<String> = skip.iter().map(|s| format!(":!{s}")).collect();
    let skip: Vec<&str> = skip.iter().map(String::as_str).collect();
    let stat = git_out(dir, &[&["diff", "--stat", commit, "HEAD", "--", "."], &skip[..]].concat()).unwrap_or_default();
    let mut diff = git_out(dir, &[&["diff", commit, "HEAD", "--", "."], &skip[..]].concat()).unwrap_or_default();
    if diff.len() > limit {
        let cut = diff.char_indices().take_while(|(i, _)| *i < limit).last().map_or(0, |(i, _)| i);
        diff.truncate(cut);
        diff.push_str("\n… (cut; see `git diff` in the project folder)");
    }
    (stat, diff)
}

/// Takes back what the step with key `key` and every later step changed, back to where the base branch
/// stood before it. The files in `own` (the humans', the run's records) stay as they are now.
pub fn take_back(dir: &Path, key: &str, own: &[String]) -> anyhow::Result<()> {
    let at = before(key);
    git(dir, &["cat-file", "-e", &at])
        .map_err(|_| anyhow!("step {key} never ran in {}: nothing to go back to", dir.display()))?;
    let keep: Vec<String> = own.iter().map(|f| format!(":!{f}")).collect();
    let keep: Vec<&str> = keep.iter().map(String::as_str).collect();
    git(dir, &["reset", "-q", &at])?;
    git(dir, &[&["checkout", "-q", "--", "."], keep.as_slice()].concat())?;
    git(dir, &[&["clean", "-fdq", "--", "."], keep.as_slice()].concat())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A git repo in a fresh temporary folder, on `main`, with an identity.
    pub(crate) fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@t"]] {
            git(dir.path(), args).unwrap();
        }
        dir
    }

    /// A step's inputs count as unchanged only when nothing under them changed since its result.
    #[test]
    fn unchanged_since_looks_only_at_the_named_paths() {
        let dir = repo();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        std::fs::write(dir.path().join("app/a.java"), "a").unwrap();
        commit(dir.path(), &["."], "result").unwrap();
        let at = git_out(dir.path(), &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(dir.path().join("notes.md"), "n").unwrap();
        commit(dir.path(), &["."], "elsewhere").unwrap();
        assert!(unchanged_since(dir.path(), &at, &["app/".into()]));
        std::fs::write(dir.path().join("app/a.java"), "b").unwrap();
        commit(dir.path(), &["."], "input").unwrap();
        assert!(!unchanged_since(dir.path(), &at, &["app/".into()]));
        assert!(changes_since(dir.path(), &at, &[], 5).1.ends_with("(cut; see `git diff` in the project folder)"));
    }

    /// Going back to before a step drops what it and later steps wrote, and keeps the humans' files.
    #[test]
    fn take_back_drops_later_results_and_keeps_the_own_files() {
        let dir = repo();
        let write = |f: &str, s: &str| std::fs::write(dir.path().join(f), s).unwrap();
        write("task.md", "old");
        write("out.txt", "before");
        commit(dir.path(), &["."], "inputs").unwrap();
        git(dir.path(), &["update-ref", &before("2.5"), "HEAD"]).unwrap();
        write("out.txt", "step 2.5");
        write("new.txt", "step 2.6");
        commit(dir.path(), &["."], "steps").unwrap();
        write("task.md", "edited by the human");
        take_back(dir.path(), "2.5", &["task.md".into()]).unwrap();
        let read = |f: &str| std::fs::read_to_string(dir.path().join(f)).ok();
        assert_eq!(read("out.txt").as_deref(), Some("before"));
        assert_eq!(read("new.txt"), None);
        assert_eq!(read("task.md").as_deref(), Some("edited by the human"));
        assert!(take_back(dir.path(), "3.1", &[]).is_err(), "a step that never ran has nothing to go back to");
    }
}
