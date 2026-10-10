//! `agg wiki lint` — check a knowledge wiki against the Open Knowledge Format, OKF v0.2
//! (<https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md>).
//!
//! The default target is the project's own `agg/state/wiki/`, which the WORKER writes and agg only
//! reads (as untrusted input). So this module READS and REPORTS; it never writes a byte into the
//! wiki — no frontmatter, no `generated`, and above all no `verified`, which is a reviewer's act.
//!
//! **Problems** are the conformance rules of §11: every non-reserved `.md` has a parseable
//! frontmatter block with a non-empty `type`, and the reserved `index.md` / `log.md` follow §8 / §9.
//! They make `ok` false and the exit code 1. **Warnings** are the producer guidance of §5 (the shape
//! of `status`, `generated`, `verified`, `sources`, timestamps) and never fail the lint, because §11
//! forbids a consumer to reject a concept over them.

mod concept;
mod reserved;

#[cfg(test)]
mod tests;

use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Bumped on any breaking change to `agg wiki lint --json` (`docs/schemas/wiki_lint.v1.json`).
pub const SCHEMA_VERSION: u32 = 1;

/// One finding, located by file (relative to the wiki root, `/`-separated) and line when known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub file: String,
    pub line: Option<usize>,
    /// a stable rule id, e.g. `type.missing`, `log.order`.
    pub rule: &'static str,
    pub message: String,
}

/// The machine-readable result of `agg wiki lint --json`.
#[derive(Debug, Serialize)]
pub struct LintReport {
    pub schema_version: u32,
    /// no problems (warnings do not count).
    pub ok: bool,
    /// the wiki directory that was checked.
    pub wiki: String,
    /// false = the default wiki does not exist yet: nothing to check, which is conformant.
    pub exists: bool,
    /// markdown files checked, reserved ones included.
    pub files: usize,
    /// of those, concept documents.
    pub concepts: usize,
    pub problems: Vec<Finding>,
    pub warnings: Vec<Finding>,
}

/// The two finding lists a check appends to.
#[derive(Debug, Default)]
pub(crate) struct Findings {
    problems: Vec<Finding>,
    warnings: Vec<Finding>,
}

impl Findings {
    fn problem(&mut self, file: &str, line: Option<usize>, rule: &'static str, message: &str) {
        self.problems.push(Finding { file: file.into(), line, rule, message: message.into() });
    }
    fn warning(&mut self, file: &str, rule: &'static str, message: &str) {
        self.warnings.push(Finding { file: file.into(), line: None, rule, message: message.into() });
    }
    fn warning_at(&mut self, file: &str, line: usize, rule: &'static str, message: &str) {
        self.warnings.push(Finding { file: file.into(), line: Some(line), rule, message: message.into() });
    }
}

/// Lint the wiki rooted at `root`. `worker_wiki` = it is the project's own, worker-written wiki,
/// where a `verified` key is a claim nobody made (a warning). Errors only when `root` is not a
/// readable directory.
pub fn lint(root: &Path, worker_wiki: bool) -> Result<LintReport> {
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let mut files = Vec::new();
    let mut out = Findings::default();
    collect(root, root, &mut files, &mut out)?;
    let mut concepts = 0;
    for path in &files {
        concepts += usize::from(check_file(root, path, worker_wiki, &mut out));
    }
    Ok(LintReport {
        schema_version: SCHEMA_VERSION,
        ok: out.problems.is_empty(),
        wiki: root.display().to_string(),
        exists: true,
        files: files.len(),
        concepts,
        problems: out.problems,
        warnings: out.warnings,
    })
}

/// The report for a default wiki that does not exist yet.
fn absent(root: &Path) -> LintReport {
    LintReport {
        schema_version: SCHEMA_VERSION,
        ok: true,
        wiki: root.display().to_string(),
        exists: false,
        files: 0,
        concepts: 0,
        problems: vec![],
        warnings: vec![],
    }
}

/// Every `.md` under `dir`, sorted, skipping dot-entries (`.obsidian/`, `.git/`). A symlink is
/// reported and NOT followed: the wiki is worker-written, and a link can loop or leave the tree.
fn collect(root: &Path, dir: &Path, files: &mut Vec<PathBuf>, out: &mut Findings) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            out.warning(&rel(root, &path), "file.symlink", "a symlink, not followed");
        } else if meta.is_dir() {
            collect(root, &path, files, out)?;
        } else if name.ends_with(".md") {
            files.push(path);
        }
    }
    Ok(())
}

/// Lint one file by what its name makes it. Returns true when it is a concept document.
fn check_file(root: &Path, path: &Path, worker_wiki: bool, out: &mut Findings) -> bool {
    let file = rel(root, path);
    let text = match std::fs::read(path).map(String::from_utf8) {
        Ok(Ok(t)) => t,
        Ok(Err(_)) => {
            out.problem(&file, None, "file.encoding", "not UTF-8: an OKF document is UTF-8 markdown (OKF §4)");
            return false;
        }
        Err(e) => {
            out.problem(&file, None, "file.unreadable", &e.to_string());
            return false;
        }
    };
    match path.file_name().and_then(|n| n.to_str()) {
        Some("index.md") => reserved::check_index(&file, &text, path.parent() == Some(root), out),
        Some("log.md") => reserved::check_log(&file, &text, out),
        _ => {
            concept::check(&file, &text, worker_wiki, out);
            return true;
        }
    }
    false
}

/// `path` relative to `root`, with `/` separators.
fn rel(root: &Path, path: &Path) -> String {
    let r = path.strip_prefix(root).unwrap_or(path);
    r.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// `agg wiki lint [PATH] [--json]`: lint `path`, or the project's `agg/state/wiki/`, print the
/// report, and return whether it is conformant. A missing default wiki is conformant (there is
/// nothing to check yet); a missing explicit `path` is an error.
pub fn run(project_dir: &Path, path: Option<&Path>, json: bool) -> Result<bool> {
    let own = crate::paths::wiki_dir(project_dir);
    let report = match path {
        None if !own.exists() => absent(&own),
        None => lint(&own, true)?,
        Some(p) => {
            if !p.exists() {
                bail!("no wiki at {}", p.display());
            }
            lint(p, same_dir(p, &own))?
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report));
    }
    Ok(report.ok)
}

/// Do two paths name the same directory?
fn same_dir(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// The human-readable report.
pub fn render(r: &LintReport) -> String {
    if !r.exists {
        return format!("no wiki at {} yet: nothing to lint\n", r.wiki);
    }
    let mut s = format!("agg wiki lint: {} (OKF v0.2)\n", r.wiki);
    for (glyph, f) in r.problems.iter().map(|f| ("✗", f)).chain(r.warnings.iter().map(|f| ("⚠", f))) {
        let at = f.line.map(|l| format!(":{l}")).unwrap_or_default();
        s.push_str(&format!("  {glyph} {}{at} [{}] {}\n", f.file, f.rule, f.message));
    }
    let tally = format!("{} file(s), {} concept(s), {} warning(s)", r.files, r.concepts, r.warnings.len());
    if r.ok {
        s.push_str(&format!("✔ conformant: {tally}\n"));
    } else {
        s.push_str(&format!("✗ {} violation(s), not OKF v0.2 conformant: {tally}\n", r.problems.len()));
    }
    s
}
