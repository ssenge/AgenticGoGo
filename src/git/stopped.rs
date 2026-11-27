//! The work a stopped session left, found again by the step it belonged to.
//!
//! A session branch (`<prefix>/<project>/session-<N>`, parked as `…/orphaned-<ts>/session-<N>` by the next run) and a
//! partial ref (`refs/rlt/partial/<run>/<session>`) carry no step name of their own, and the ledger only records a
//! step once it finished. So rlt writes the step into the repository's config when the branch is cut or the ref
//! saved: `branch.<name>.rlt-step` (git moves a branch's config section along when the branch is renamed, as parking
//! does) and `rlt-partial.<ref>.step`. rlt only reads this to tell a later session of the step; it never merges it.

use super::git;
use std::path::Path;

/// How many stopped sessions' work a brief names, newest first.
pub const LIMIT: usize = 3;
/// How many lines of `git diff --stat` each gets.
const STAT_LINES: usize = 40;
/// The refs' own runtime state is never work.
const RUNTIME: [&str; 2] = [":(exclude)rlt/state/**", ":(exclude)rlt/private/**"];

/// One stopped session's work: its ref, the commit it stands on and what it changed against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub name: String,
    pub base: String,
    pub stat: String,
}

/// Remembers that session branch `branch` runs step `step`.
pub fn note_branch_step(dir: &Path, branch: &str, step: &str) {
    let _ = git(dir, &["config", &format!("branch.{branch}.rlt-step"), step]);
}

/// Remembers that partial ref `name` holds work of step `step`.
pub fn note_ref_step(dir: &Path, name: &str, step: &str) {
    let _ = git(dir, &["config", &format!("rlt-partial.{name}.step"), step]);
}

/// The step a stopped session's ref belongs to, if rlt noted it.
fn step_of(dir: &Path, name: &str) -> Option<String> {
    let key = match name.strip_prefix("refs/heads/") {
        Some(branch) => format!("branch.{branch}.rlt-step"),
        None => format!("rlt-partial.{name}.step"),
    };
    let (ok, out, _) = git(dir, &["config", "--get", &key]);
    (ok && !out.is_empty()).then_some(out)
}

/// Whether `name` is a ref a stopped session can have left: a partial ref, or an orphaned (parked) session branch.
fn is_stopped_ref(name: &str) -> bool {
    name.starts_with("refs/rlt/partial/") || (name.starts_with("refs/heads/") && name.contains("/orphaned-"))
}

/// The work of the (at most [`LIMIT`]) newest stopped sessions of step `step`, whose changes HEAD doesn't have yet.
pub fn stopped_work(dir: &Path, step: &str) -> Vec<Stopped> {
    let (ok, refs, _) = git(dir, &["for-each-ref", "--sort=-committerdate", "--format=%(refname)", "refs/heads", "refs/rlt/partial"]);
    if !ok {
        return Vec::new();
    }
    let mut found: Vec<Stopped> = Vec::new();
    for name in refs.lines().filter(|n| is_stopped_ref(n) && step_of(dir, n).as_deref() == Some(step)) {
        let (merged, base, _) = git(dir, &["merge-base", "HEAD", name]);
        let (_, files, _) = git(dir, &["diff", "--name-only", &base, name, "--", "."]);
        let files: Vec<&str> = files.lines().filter(|f| !f.starts_with("rlt/state/") && !f.starts_with("rlt/private/")).collect();
        let mut taken = vec!["diff", "--quiet", "HEAD", name, "--"];
        taken.extend(&files);
        // nothing of it left to take: the session changed nothing, or HEAD has every file as the session left it
        if !merged || files.is_empty() || git(dir, &taken).0 {
            continue;
        }
        let mut stat = vec!["diff", "--stat", &base, name, "--", "."];
        stat.extend(RUNTIME);
        let (_, stat, _) = git(dir, &stat);
        let shown = name.strip_prefix("refs/heads/").unwrap_or(name).to_string();
        found.push(Stopped { name: shown, base: base.chars().take(12).collect(), stat: truncated(&stat) });
        if found.len() == LIMIT {
            break;
        }
    }
    found
}

/// `stat` cut to [`STAT_LINES`] lines, saying how many it left out.
pub(super) fn truncated(stat: &str) -> String {
    let lines: Vec<&str> = stat.lines().collect();
    if lines.len() <= STAT_LINES {
        return stat.to_string();
    }
    format!("{}\n… ({} more lines)", lines[..STAT_LINES].join("\n"), lines.len() - STAT_LINES)
}
