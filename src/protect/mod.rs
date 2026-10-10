//! Caller-defined PROTECTED PATHS (`write_deny`): globs the worker may read but never change.
//!
//! # Two layers, and which one is the guarantee
//! 1. **The kernel** (first line): under `isolation: sandbox`/`container` the patterns reach the
//!    same mechanism as `agg/private/` — Seatbelt deny rules on macOS, read-only binds under bwrap
//!    and in a container. A write is refused with `EPERM`/`EROFS` before it happens.
//! 2. **The gate check** (the guarantee): a content-hash [`snapshot`] of every covered entry —
//!    tracked, untracked AND gitignored — is taken before the worker starts and compared when it
//!    exits. Any difference rolls the session back, the paths are put back from agg's own copies,
//!    and a `protected_path_violation` row lands in `agg/private/violations.jsonl`. This works on
//!    EVERY tier, `none` included, whatever the sandbox did or failed to do.
//!
//! The kernel layer is what keeps a session's work from being thrown away for one stray write; the
//! gate is what makes "the worker cannot change an oracle" true even where no kernel layer exists.
//!
//! # Union, never subtraction
//! A step's list is ADDED to `defaults.write_deny`. Unlike `readonly:`, nothing subtracts from it —
//! `writable:` does not reach these patterns — so a step can only ever be MORE restricted than the
//! project default. That is what lets an operator rely on the default list.

pub mod glob;
pub mod sha256;
pub mod snapshot;

use crate::isolation::Isolation;
use anyhow::Result;
use glob::{Glob, Shape};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The reason string a violation is recorded under — in the ledger, on stderr, and in reports.
pub const VIOLATION: &str = "protected_path_violation";

/// A compiled `write_deny` list plus how this project's filesystem compares names.
#[derive(Debug, Clone)]
pub struct PatternSet {
    pub globs: Vec<Glob>,
    /// the project lives on a case-insensitive filesystem (the macOS default), so `TESTS/x` IS
    /// `tests/x` and must match like it.
    pub ci: bool,
}

impl PatternSet {
    /// Compile `patterns` for the project at `root`. Every pattern must parse; startup validation
    /// ([`check_step`]) has already said so in the operator's terms, so an error here is a bug
    /// path, not a config path.
    pub fn compile(root: &Path, patterns: &[String]) -> Result<PatternSet> {
        let globs = patterns.iter().map(|p| Glob::parse(p)).collect::<Result<Vec<_>>>()?;
        Ok(PatternSet { globs, ci: case_insensitive(root) })
    }
    pub fn is_empty(&self) -> bool {
        self.globs.is_empty()
    }
    pub fn matches(&self, rel: &str) -> bool {
        self.globs.iter().any(|g| g.matches(rel, self.ci))
    }
    pub fn could_match_below(&self, dir_rel: &str) -> bool {
        self.globs.iter().any(|g| g.could_match_below(dir_rel, self.ci))
    }
}

/// Normalise a `write_deny` list: each pattern to its one canonical spelling, duplicates dropped.
/// A pattern that does not parse is kept VERBATIM, so [`check_step`] can refuse it by name rather
/// than this function dropping it quietly — dropping a protection is the unsafe direction.
pub fn normalize<I, S>(patterns: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for p in patterns {
        let s = Glob::parse(p.as_ref()).map(|g| g.text).unwrap_or_else(|_| p.as_ref().to_string());
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// `defaults ∪ step`, normalised — the only way a step's protected set is ever formed.
pub fn union(defaults: &[String], step: &[String]) -> Vec<String> {
    normalize(defaults.iter().chain(step))
}

/// Does `root` sit on a case-insensitive filesystem? Probed by looking an existing entry up under
/// its case-swapped name and comparing inodes — no file is written. Every agg project has an `agg/`
/// directory, so there is always an entry with a letter in it to probe.
pub fn case_insensitive(root: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(root) else { return false };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let swapped: String = name
            .chars()
            .map(|c| if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() })
            .collect();
        if swapped == name {
            continue;
        }
        let (Ok(a), Ok(b)) = (std::fs::symlink_metadata(e.path()), std::fs::symlink_metadata(root.join(&swapped)))
        else {
            return false;
        };
        return same_file(&a, &b);
    }
    false
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}
#[cfg(not(unix))]
fn same_file(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
    true
}

// ---------------------------------------------------------------------------------------------
// the platform matrix
// ---------------------------------------------------------------------------------------------

/// Does this tier enforce writes by READ-ONLY BIND of a concrete path (bwrap, a container) rather
/// than by path rule (Seatbelt)? A bind can only cover paths that exist when the session starts.
pub fn binds(tier: Isolation) -> bool {
    match tier {
        Isolation::None => false,
        Isolation::Container => true,
        Isolation::Sandbox => !cfg!(target_os = "macos"),
    }
}

/// Validate one step's protected set for its tier — the startup refusal (A-5 "refuse rather than
/// degrade"). `Ok(Some(w))` = runs, with a warning to print (bare text, no `⚠` prefix); `Err(problems)` = refuse to start.
///
/// | tier | outcome |
/// |---|---|
/// | `none` | runs; WARNS that only the gate check applies (nothing stops the write itself) |
/// | `sandbox` on macOS | runs; every pattern becomes a Seatbelt rule |
/// | `sandbox` on Linux, `container` | runs if every pattern is a literal path or `dir/**`; a deeper wildcard is REFUSED |
pub fn check_step(step: &str, tier: Isolation, patterns: &[String]) -> Result<Option<String>, Vec<String>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut problems = Vec::new();
    for p in patterns {
        match Glob::parse(p) {
            Err(e) => problems.push(format!("step `{step}`: write_deny {e}")),
            Ok(g) if binds(tier) && g.shape() == Shape::Other => problems.push(format!(
                "step `{step}`: write_deny `{}` cannot be enforced by the kernel under \
                 `isolation: {}` on this platform — it denies writes by read-only bind of a path \
                 that exists at session start, and this pattern can name files that do not exist yet \
                 in directories it does not list.\n      \
                 fix: protect the directories that hold them (`dir/**`) or list literal paths.",
                g.text,
                tier_name(tier),
            )),
            Ok(_) => {}
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok((tier == Isolation::None).then(|| {
        format!(
            "step `{step}` protects {} under `isolation: none` — only the GATE CHECK applies: \
             nothing stops the write itself, but a session that changed a protected path is rolled \
             back and the path restored. Set `isolation: sandbox` to also deny the write.",
            patterns.join(", ")
        )
    }))
}

fn tier_name(t: Isolation) -> &'static str {
    match t {
        Isolation::None => "none",
        Isolation::Sandbox => "sandbox",
        Isolation::Container => "container",
    }
}

/// The Seatbelt rules for `patterns` under the canonical project `root` (macOS `sandbox` tier).
///
/// One `(deny file-write* (regex …))` per pattern — every kind of write: data, create, unlink,
/// rename, `chmod`, `utimes`, and a hard link FROM the protected file. Plus one
/// `(deny file-write-unlink …)` per ANCESTOR of each pattern's literal directory, inside the
/// project: without it `mv tests tests_old` carries a protected `tests/oracles/` out from under its
/// own rule (measured on a real host). Creating and deleting OTHER entries in those ancestors stays
/// allowed — only removing or renaming the ancestor itself is denied.
pub fn seatbelt_rules(root: &Path, patterns: &[String]) -> Result<Vec<String>> {
    let set = PatternSet::compile(root, patterns)?;
    let root_s = root.to_string_lossy();
    let mut rules = Vec::new();
    for g in &set.globs {
        rules.push(format!("(deny file-write* (regex #\"{}\"))", g.to_regex(&root_s, set.ci)));
        let prefix = g.literal_prefix();
        // every PROPER ancestor of what the pattern matches. For a literal path or `dir/**` the last
        // literal component is the match itself (the regex already covers it); for anything deeper
        // (`tests/*/x.json`) the whole literal prefix lies above every match.
        let upto = match g.shape() {
            Shape::Literal(_) | Shape::Subtree(_) => prefix.len().saturating_sub(1),
            Shape::Other => prefix.len(),
        };
        for i in 1..=upto {
            let anc = prefix[..i].join("/");
            let re = format!(
                "^{}/{}$",
                glob::regex_literal(root_s.trim_end_matches('/'), false),
                glob::regex_literal(&anc, set.ci)
            );
            let rule = format!("(deny file-write-unlink (regex #\"{re}\"))");
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
    }
    Ok(rules)
}

/// The concrete project-relative paths a BIND-based tier re-mounts read-only for `patterns`.
///
/// `dir/**` ⇒ `dir` (agg creates it first if absent — see [`materialize`]); a literal path ⇒ that
/// path if it exists. A literal path that does NOT exist yet has no mount point and is left to the
/// gate check, with a warning: binding its parent would deny the whole directory, and creating it
/// would mean guessing whether it is meant to be a file or a directory. [`check_step`] has already
/// refused every other shape.
pub fn bind_paths(root: &Path, patterns: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for p in patterns {
        match Glob::parse(p).map(|g| g.shape()) {
            Ok(Shape::Subtree(d)) => out.push(d),
            Ok(Shape::Literal(l)) if root.join(&l).symlink_metadata().is_ok() => out.push(l),
            Ok(Shape::Literal(l)) => eprintln!(
                "  ⚠ write_deny `{l}` does not exist yet, so a read-only bind has nothing to mount — \
                 creating it is caught by the gate check, not denied by the kernel"
            ),
            _ => {}
        }
    }
    out
}

/// Create every `dir/**` root that does not exist yet, BEFORE the session's snapshot, on a tier
/// that [`binds`]: the bind needs a mount point, and a directory agg creates after the snapshot
/// would read as the worker's doing.
pub fn materialize(root: &Path, tier: Isolation, patterns: &[String]) {
    if !binds(tier) {
        return;
    }
    for p in patterns {
        if let Ok(Shape::Subtree(d)) = Glob::parse(p).map(|g| g.shape()) {
            let _ = std::fs::create_dir_all(root.join(d));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the ledger
// ---------------------------------------------------------------------------------------------

/// One row of `agg/private/violations.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViolationRecord {
    /// the session whose worker changed the paths (for `between_sessions`, the one before).
    pub session: u32,
    pub step: String,
    /// always [`VIOLATION`] — a field rather than implied, so a reader greps one stable token.
    pub reason: String,
    /// `"session"`: during a worker session (the session is rolled back). `"between_sessions"`:
    /// after the check and before the next session — a process the worker left behind, a hook, a
    /// judge; the paths are restored, but the work it rode in on had already been gated.
    pub window: String,
    /// project-relative, sorted.
    pub paths: Vec<String>,
    /// whether agg put every path back. `false` is followed by a hard stop of the run.
    pub restored: bool,
    pub ts: u64,
}

/// `agg/private/violations.jsonl` ([`crate::paths::violations_jsonl`]).
pub fn ledger_path(dir: &Path) -> PathBuf {
    crate::paths::violations_jsonl(dir)
}

/// Append one row, fsynced. A failed write is an error the caller surfaces — the row is the record
/// that a session was rolled back for this reason.
pub fn append(dir: &Path, rec: &ViolationRecord) -> Result<()> {
    use std::io::Write;
    let p = ledger_path(dir);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p)?;
    writeln!(f, "{}", serde_json::to_string(rec)?)?;
    f.sync_all()?;
    Ok(())
}

/// Every recorded violation, oldest first. A missing or unreadable file is "none".
pub fn read(dir: &Path) -> Vec<ViolationRecord> {
    std::fs::read_to_string(ledger_path(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests;
