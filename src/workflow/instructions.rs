//! What a step was told when it gave its result (SR-P3): kept per step in `agg/private/instructions/`, so a later
//! call can say how its instructions changed since. A `--from` after a prompt change otherwise hands the worker
//! its whole task beside a result that looks done, and it keeps the result as it is.

use std::path::{Path, PathBuf};
use std::process::Command;

/// How much of the instructions' changes a step gets in its brief.
const DIFF_LIMIT: usize = 8_000;

/// Where the instructions of the step with key `key` are kept.
fn kept(project: &Path, key: &str) -> PathBuf {
    crate::paths::private_dir(project).join("instructions").join(format!("{key}.md"))
}

/// Keeps `text` as what the step with key `key` was told for the result it just gave.
pub(crate) fn keep(project: &Path, key: &str, text: &str) {
    let path = kept(project, key);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, text);
}

/// How the instructions `now` differ from those the step's last result was given, as a diff; `None` when they
/// are the same or none were kept (a step that gave no result yet, or one from before agg kept them).
pub(crate) fn changed(project: &Path, key: &str, now: &str) -> Option<String> {
    let before = kept(project, key);
    let earlier = std::fs::read_to_string(&before).ok()?;
    if earlier == now {
        return None;
    }
    let current = before.with_extension("now.md");   // beside the kept one: no temporary folder to clean up
    std::fs::write(&current, now).ok()?;
    let out = Command::new("git").args(["diff", "--no-index", "--no-color", "-U2", "--"])
        .arg(&before).arg(&current).output();
    let _ = std::fs::remove_file(&current);
    let out = out.ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut diff: String = text.lines().skip_while(|l| !l.starts_with("@@")).collect::<Vec<_>>().join("\n");
    if diff.len() > DIFF_LIMIT {
        let cut = diff.char_indices().take_while(|(i, _)| *i < DIFF_LIMIT).last().map_or(0, |(i, _)| i);
        diff.truncate(cut);
        diff.push_str("\n… (cut)");
    }
    Some(diff)
}

/// The brief's section for instructions that changed by `diff` since the result in place.
pub(crate) fn section(diff: &str) -> String {
    format!("## Your instructions changed since your result\n\nYour result in place was made under earlier \
             instructions of this step. Since then they changed as follows:\n\n```diff\n{diff}\n```\n\nBring your \
             result in line with what changed, and change nothing else.")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing kept: no change to tell; kept and the same: none; kept and changed: the changed lines.
    #[test]
    fn a_change_of_the_instructions_is_told_as_a_diff() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(changed(dir.path(), "2.10", "Write the rules.\n"), None);
        keep(dir.path(), "2.10", "Write the rules.\nKeep them short.\n");
        assert_eq!(changed(dir.path(), "2.10", "Write the rules.\nKeep them short.\n"), None);
        let diff = changed(dir.path(), "2.10", "Write the rules.\nWord them for any platform.\n").unwrap();
        assert!(diff.contains("-Keep them short.") && diff.contains("+Word them for any platform."), "{diff}");
        assert!(!diff.contains("instructions/2.10.md"), "the paths of the two files stay out: {diff}");
        assert!(section(&diff).contains("Bring your result in line"));
    }
}
