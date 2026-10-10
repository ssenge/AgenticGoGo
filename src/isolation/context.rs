//! READ-ONLY CONTEXT DIRECTORIES (`context:`) — knowledge that lives outside the project, which the
//! worker and its judges read every session and must never change.
//!
//! agg does not interpret the contents. It resolves each entry, refuses one it cannot enforce, and
//! hands the run's confining tiers the list ([`super::policy::RunPolicy::contexts`]):
//!
//! | tier | where the worker sees it | what keeps it read-only |
//! |---|---|---|
//! | `sandbox` (macOS) | its host path | a Seatbelt `file-write*` deny on the directory, plus an unlink deny on each ancestor so it cannot be renamed away and replaced |
//! | `sandbox` (Linux) | its host path | a `bwrap --ro-bind` of the directory (after the `/tmp` tmpfs, so one under `/tmp` stays visible) |
//! | `container` | `/context/<as>` | a `:ro` bind mount |
//! | `none` | — | nothing, so a run with a context and an unconfined step is **refused** |
//!
//! Refused at startup: a path that does not exist or is not a directory; one that overlaps the
//! project (which also covers `agg/private/`) — inside it, a worker-writable copy would shadow the
//! read-only one, and containing it, the deny would take the whole project with it; a malformed or
//! duplicate `as:`.

use super::policy::ContextMount;
use crate::core::config::ContextDir;
use crate::problem::Problem;
use std::path::{Path, PathBuf};

/// Resolve every `context:` entry against the project dir, collecting each problem at its key.
/// The mounts are returned only for entries that resolved; a non-empty problem list refuses the run.
pub fn resolve(entries: &[ContextDir], project: &Path) -> (Vec<ContextMount>, Vec<Problem>) {
    let project = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let mut mounts: Vec<ContextMount> = Vec::new();
    let mut problems = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let at = |k: &str| format!("context[{i}].{k}");
        if let Some(why) = bad_name(&e.name) {
            problems.push(Problem::new(at("as"), why));
            continue;
        }
        let env = ContextMount { name: e.name.clone(), host: PathBuf::new() }.env_key();
        if mounts.iter().any(|m| m.env_key() == env) || entries[..i].iter().any(|p| p.name == e.name) {
            problems.push(Problem::new(at("as"), format!("context name `{}` is used twice — each `as:` must be unique", e.name)));
            continue;
        }
        match host_dir(&e.path, &project) {
            Ok(host) => mounts.push(ContextMount { name: e.name.clone(), host }),
            Err(why) => problems.push(Problem::new(at("path"), why)),
        }
    }
    (mounts, problems)
}

/// Why `name` is not a usable `as:`, or `None`. Lowercase, digits, `-` and `_`, starting with a
/// letter or digit: it becomes a path component (`/context/<as>`) and an env var suffix, and both
/// must mean exactly one thing.
fn bad_name(name: &str) -> Option<String> {
    let ok_first = name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let ok_rest = name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok_first && ok_rest && name.len() <= 64 {
        return None;
    }
    Some(format!(
        "context name `{name}` is not a short name — use lowercase letters, digits, `-` and `_` \
         (at most 64, starting with a letter or digit); it becomes `/context/<as>` and `AGG_CONTEXT_<AS>`"
    ))
}

/// The canonical directory `raw` names, or why it cannot be a context.
fn host_dir(raw: &str, project: &Path) -> Result<PathBuf, String> {
    let raw_t = raw.trim();
    if raw_t.is_empty() {
        return Err("a context needs a `path`".into());
    }
    let p = expand(raw_t, project);
    let host = std::fs::canonicalize(&p).map_err(|e| format!("context `{raw}` does not exist ({e}) — create it, or fix the path"))?;
    if !host.is_dir() {
        return Err(format!("context `{raw}` is not a directory"));
    }
    if host.starts_with(project) || project.starts_with(&host) {
        return Err(format!(
            "context `{raw}` overlaps the project ({}) — a context is read-only material from OUTSIDE \
             it: inside, the worker could shadow it with a writable copy (and `agg/private/` is \
             agg's); containing it, the read-only rule would take the project with it",
            project.display()
        ));
    }
    Ok(host)
}

/// `~/x` → `$HOME/x`; a relative path → against the project; an absolute one as written.
pub(super) fn expand(raw: &str, project: &Path) -> PathBuf {
    if let (Some(rest), Some(home)) = (raw.strip_prefix("~/"), std::env::var_os("HOME")) {
        return Path::new(&home).join(rest);
    }
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        project.join(p)
    }
}

/// The brief's `## Context` section for a worker on `tier`, or "" when the run has none. Points at
/// each directory's `index.md` when it has one — agg never reads the contents.
pub fn brief_section(contexts: &[ContextMount], tier: super::Isolation) -> String {
    if contexts.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n## Context (read-only)\nReference material for this project. READ it as you need it; it is \
         read-only — never try to change it (writes are refused).\n",
    );
    for c in contexts {
        let seen = c.seen_on(tier);
        s.push_str(&format!("- `{}` — `{}`", c.name, seen.display()));
        if c.host.join("index.md").is_file() {
            s.push_str(&format!(" — start at `{}`", seen.join("index.md").display()));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ContextMode;

    fn entry(path: &Path, name: &str) -> ContextDir {
        ContextDir { path: path.to_string_lossy().into_owned(), name: name.into(), mode: ContextMode::Ro }
    }

    fn paths(problems: &[Problem]) -> Vec<&str> {
        problems.iter().map(|p| p.path.as_str()).collect()
    }

    #[test]
    fn a_good_context_resolves_to_its_canonical_dir() {
        let proj = tempfile::tempdir().unwrap();
        let brain = tempfile::tempdir().unwrap();
        let (m, p) = resolve(&[entry(brain.path(), "brain")], proj.path());
        assert!(p.is_empty(), "{p:?}");
        assert_eq!(m, [ContextMount { name: "brain".into(), host: std::fs::canonicalize(brain.path()).unwrap() }]);
    }

    /// Each refusal the requirement names, located at the entry's own key.
    #[test]
    fn every_unenforceable_entry_is_refused_where_it_is_written() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join("docs")).unwrap();
        std::fs::create_dir_all(proj.path().join("agg/private")).unwrap();
        let brain = tempfile::tempdir().unwrap();
        let file = brain.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        let entries = [
            entry(brain.path(), "brain"),
            entry(brain.path(), "brain"),                         // duplicate `as`
            entry(&brain.path().join("nope"), "missing"),         // does not exist
            entry(&file, "file"),                                 // not a directory
            entry(&proj.path().join("docs"), "inside"),           // inside the project
            entry(&proj.path().join("agg/private"), "private"),   // agg's own state (inside)
            entry(proj.path().parent().unwrap(), "around"),       // contains the project
            entry(brain.path(), "Bad Name"),                      // malformed `as`
        ];
        let (m, p) = resolve(&entries, proj.path());
        assert_eq!(m.len(), 1, "only the first entry resolves: {m:?}");
        assert_eq!(
            paths(&p),
            ["context[1].as", "context[2].path", "context[3].path", "context[4].path", "context[5].path", "context[6].path", "context[7].as"],
            "{p:#?}"
        );
        assert!(p[4].message.contains("overlaps the project"), "{}", p[4].message);
    }

    /// `a-b` and `a_b` would both be `AGG_CONTEXT_A_B` — one variable for two directories.
    #[test]
    fn names_that_collide_as_env_vars_are_duplicates() {
        let proj = tempfile::tempdir().unwrap();
        let a = tempfile::tempdir().unwrap();
        let (_, p) = resolve(&[entry(a.path(), "a-b"), entry(a.path(), "a_b")], proj.path());
        assert_eq!(paths(&p), ["context[1].as"]);
    }

    #[test]
    fn a_relative_or_home_path_is_expanded() {
        let proj = Path::new("/p");
        assert_eq!(expand("../kb", proj), Path::new("/p/../kb"));
        assert_eq!(expand("/abs", proj), Path::new("/abs"));
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand("~/kb", proj), Path::new(&home).join("kb"));
    }

    #[test]
    fn the_brief_lists_each_context_where_the_tier_shows_it_with_its_index() {
        let brain = tempfile::tempdir().unwrap();
        std::fs::write(brain.path().join("index.md"), "# idx").unwrap();
        let docs = tempfile::tempdir().unwrap();
        let ctx = [
            ContextMount { name: "brain".into(), host: brain.path().to_path_buf() },
            ContextMount { name: "docs".into(), host: docs.path().to_path_buf() },
        ];
        let s = brief_section(&ctx, super::super::Isolation::Container);
        assert!(s.contains("## Context (read-only)"), "{s}");
        assert!(s.contains("`brain` — `/context/brain` — start at `/context/brain/index.md`"), "{s}");
        assert!(s.contains("`docs` — `/context/docs`\n"), "no index ⇒ no pointer: {s}");
        let s = brief_section(&ctx, super::super::Isolation::Sandbox);
        assert!(s.contains(&format!("`{}`", brain.path().display())), "the host path outside a container: {s}");
        assert_eq!(brief_section(&[], super::super::Isolation::Sandbox), "");
    }
}
