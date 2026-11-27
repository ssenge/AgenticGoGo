//! Verifiers resolved by NAME, from disk — there is no registry (§5.1).
//!
//! ```text
//! all_tests_pass
//!   1. rlt/verifiers/all_tests_pass.sh   → script verifier      (THIS project's — shadows everything)
//!   2. rlt/verifiers/all_tests_pass.md   → rubric verifier; the FILE IS THE RUBRIC
//!   3. ~/.rlt/verifiers/all_tests_pass.* → the STANDARD LIBRARY (below)
//!   4. else → HARD ERROR AT STARTUP, listing what IS available
//! ```
//! The **extension decides the kind**: `.sh` = script, `.md` = rubric ⇒ LLM. An `.md` verifier
//! declares its `inputs:` in its OWN yaml frontmatter — one self-contained file, no registry.
//!
//! # The standard library ships INSIDE the binary (§6.1)
//! The verifiers are `include_str!`'d — exactly as [`crate::skills`] does the `/rlt:*` skills — and
//! rlt writes them to `~/.rlt/verifiers/` on `rlt init`, and on `rlt run` when a file is missing or
//! has DRIFTED from the embedded copy. Install rlt → the verifiers are installed. No `rlt verifiers`
//! subcommand, no HTTP client, no network.
//!
//! # Security (§6.1)
//! `~/.rlt/verifiers/` is worker-writable and outside git/rollback: one session that writes there
//! corrupts the grader for every project on the machine. So (a) rlt NEVER hands the worker that
//! path, and (b) [`ensure_library`] verifies every library file against its embedded copy on
//! startup and rewrites any drift.

use crate::core::model::VerifierKind;
use anyhow::{bail, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The standard verifier library, embedded at compile time from the same `plugin/verifiers/` files the
/// Claude plugin ships. `(filename, body)`. Parameterless by convention (§5.1); anything needing an
/// argument is a three-line script in the project's own `rlt/verifiers/`.
pub const LIBRARY: &[(&str, &str)] = &[
    ("cargo_test.sh", include_str!("../../plugin/verifiers/cargo_test.sh")),
    ("build_ok.sh", include_str!("../../plugin/verifiers/build_ok.sh")),
    ("lint_clean.sh", include_str!("../../plugin/verifiers/lint_clean.sh")),
    ("git_clean.sh", include_str!("../../plugin/verifiers/git_clean.sh")),
    ("no_regression.sh", include_str!("../../plugin/verifiers/no_regression.sh")),
    ("stalled.sh", include_str!("../../plugin/verifiers/stalled.sh")),
    ("cmd_exit.sh", include_str!("../../plugin/verifiers/cmd_exit.sh")),
    ("grep_count.sh", include_str!("../../plugin/verifiers/grep_count.sh")),
];

/// `~/.rlt/verifiers/` — the user-home library dir. `None` if `$HOME` is unset.
pub fn home_verifiers_dir() -> Option<PathBuf> {
    std::env::home_dir().map(|h| h.join(".rlt").join("verifiers"))
}

/// A project's own verifiers dir: `<config_base>/verifiers/` (i.e. `rlt/verifiers/`, COMMITTED).
pub fn project_verifiers_dir(config_base: &Path) -> PathBuf {
    config_base.join("verifiers")
}

/// Write the embedded library to `~/.rlt/verifiers/`, (re)writing any file that is missing or has
/// DRIFTED from its embedded copy (§6.1 security). Best-effort: a home dir we cannot write is a
/// warning, not a hard failure — the loop still runs against project verifiers.
pub fn ensure_library() -> Result<()> {
    let Some(dir) = home_verifiers_dir() else {
        eprintln!("  ⚠ no home directory — skipping the ~/.rlt/verifiers standard library install");
        return Ok(());
    };
    std::fs::create_dir_all(&dir)?;
    for (name, body) in LIBRARY {
        let path = dir.join(name);
        // rewrite iff missing or drifted — the worker can write here, so the embedded copy is the
        // authority and any local edit is reverted.
        let drifted = std::fs::read_to_string(&path).map(|cur| cur != *body).unwrap_or(true);
        if drifted {
            std::fs::write(&path, body)?;
            make_executable(&path);
        }
    }
    Ok(())
}

/// Resolve a verifier NAME to its runnable [`VerifierKind`], searching (in order) the project's
/// `rlt/verifiers/`, then `~/.rlt/verifiers/`. `.sh` before `.md` at each level. HARD ERROR listing what
/// exists if nothing resolves (§5.1 step 4).
pub fn resolve(name: &str, config_base: &Path) -> Result<VerifierKind> {
    let proj = project_verifiers_dir(config_base);
    let home = home_verifiers_dir();
    // search order: project .sh, project .md, home .sh, home .md.
    let mut candidates: Vec<PathBuf> =
        vec![proj.join(format!("{name}.sh")), proj.join(format!("{name}.md"))];
    if let Some(h) = &home {
        candidates.push(h.join(format!("{name}.sh")));
        candidates.push(h.join(format!("{name}.md")));
    }
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        return Ok(kind_for(&path));
    }
    bail!(
        "no verifier named `{name}` — resolved neither rlt/verifiers/{name}.{{sh,md}} nor \
         ~/.rlt/verifiers/{name}.{{sh,md}}.\n  available verifiers: {}",
        available(config_base).join(", ")
    );
}

/// The [`VerifierKind`] for a resolved file: extension decides. An `.md` reads its `inputs:` from its
/// own frontmatter here, at resolution time.
pub(crate) fn kind_for(path: &Path) -> VerifierKind {
    if path.extension().and_then(|e| e.to_str()) == Some("md") {
        let inputs = std::fs::read_to_string(path)
            .ok()
            .map(|t| parse_inputs(&crate::core::vars::apply(&t).unwrap_or(t)))
            .unwrap_or_default();
        VerifierKind::Llm { path: path.to_path_buf(), inputs }
    } else {
        VerifierKind::Script { path: path.to_path_buf() }
    }
}

/// Every verifier name that resolves for `config_base` — project verifiers plus the library — for the
/// "here is what exists" hint on a resolution failure.
pub fn available(config_base: &Path) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push_dir = |dir: &Path| {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                match p.extension().and_then(|x| x.to_str()) {
                    Some("sh") | Some("md") => {
                        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                            if !names.iter().any(|n| n == stem) {
                                names.push(stem.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    push_dir(&project_verifiers_dir(config_base));
    if let Some(h) = home_verifiers_dir() {
        push_dir(&h);
    }
    names.sort();
    names
}

#[derive(Deserialize, Default)]
struct Frontmatter {
    #[serde(default)]
    inputs: Vec<String>,
}

/// Parse the `inputs:` list from an `.md` verifier's yaml frontmatter (the leading `---`…`---` block).
/// No frontmatter, or no `inputs:`, ⇒ an empty list (a rubric verifier that reads nothing).
fn parse_inputs(md: &str) -> Vec<String> {
    let t = md.trim_start_matches('\u{feff}').trim_start();
    let Some(rest) = t.strip_prefix("---") else { return vec![] };
    // the frontmatter ends at the next line that is exactly `---`.
    let Some(end) = rest.find("\n---") else { return vec![] };
    serde_yaml::from_str::<Frontmatter>(&rest[..end]).map(|f| f.inputs).unwrap_or_default()
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}
#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
