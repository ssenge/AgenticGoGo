//! Evidence persistence: keep the files a verdict names as `evidence`, after the judge that wrote
//! them is gone.
//!
//! A judge's working files live in its per-session scratch (`$AGG_JUDGE_SCRATCH`, under `$TMPDIR`),
//! which nothing keeps: by the time a human asks why a gate passed, the SARIF file or the diff report
//! the verdict pointed at may already be swept. So after judging, agg copies each declared file to
//! `agg/private/evidence/<run>/<session>/<judge>/` — agg-private, because it is part of what agg
//! believes happened.
//!
//! # What may be copied
//! A declared path is resolved against the judge's scratch first, then the project root (an absolute
//! path is taken as is). The CANONICAL path — every symlink resolved — must lie inside one of those
//! two roots, and must be a regular file. Anything else is refused and recorded, never followed: a
//! verdict is judge output, and a judge reads worker-written content, so `"../../.ssh/id_rsa"` or a
//! scratch symlink to `/etc` must not become a copy agg makes on its behalf.
//!
//! # The size cap
//! A run-wide byte budget ([`Budget`]) bounds the copies. It is a robustness bound against filling
//! the disk, not a run limit: hitting it skips the file, records why, and the run goes on.

use std::path::{Component, Path, PathBuf};

/// The default byte budget for one run's evidence copies (`judge.evidence_max_mb`).
pub const DEFAULT_MAX_MB: u64 = 100;

/// What became of one verdict's declared evidence.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Kept {
    /// where each kept copy now lives, in declaration order.
    pub saved: Vec<PathBuf>,
    /// `<declared>: <why it was not kept>`, one per refused or failed entry.
    pub skipped: Vec<String>,
}

/// The run's evidence byte budget: how much was copied, out of how much is allowed.
#[derive(Debug, Clone)]
pub struct Budget {
    pub used: u64,
    pub cap: u64,
    /// the cap refused at least one file this run — so the warning is printed once, not per file.
    pub hit: bool,
}

impl Budget {
    /// A budget of `max_mb` mebibytes.
    pub fn mb(max_mb: u64) -> Budget {
        Budget { used: 0, cap: max_mb.saturating_mul(1024 * 1024), hit: false }
    }
}

/// The two directories a declared evidence path may resolve into.
#[derive(Debug, Clone)]
pub struct Roots {
    /// the judge's per-session scratch.
    pub scratch: PathBuf,
    /// the project root.
    pub project: PathBuf,
}

/// Copy every file `declared` names into `dest`, within `budget`. Never fails: each entry that
/// cannot be kept is recorded in [`Kept::skipped`] with its reason.
pub fn persist(declared: &[String], roots: &Roots, dest: &Path, budget: &mut Budget) -> Kept {
    let mut kept = Kept::default();
    for entry in declared {
        match persist_one(entry, roots, dest, budget) {
            Ok(path) => kept.saved.push(path),
            Err(why) => kept.skipped.push(format!("{entry}: {why}")),
        }
    }
    kept
}

/// One declared entry: resolve, confine, size-check, copy. `Err` carries the reason it was skipped.
fn persist_one(entry: &str, roots: &Roots, dest: &Path, budget: &mut Budget) -> Result<PathBuf, String> {
    let (source, rel) = confine(entry, roots)?;
    let meta = std::fs::metadata(&source).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    if budget.used.saturating_add(meta.len()) > budget.cap {
        budget.hit = true;
        return Err(format!("evidence size cap reached ({} MB per run)", budget.cap / (1024 * 1024)));
    }
    let target = dest.join(&rel);
    if target.exists() {
        return Err(format!("another entry already saved `{}`", rel.display()));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(&source, &target).map_err(|e| e.to_string())?;
    budget.used += meta.len();
    Ok(target)
}

/// Resolve `entry` to a canonical source inside one of the roots, and the path RELATIVE to that
/// root it is saved under. The relative path is what keeps two files of the same name apart.
fn confine(entry: &str, roots: &Roots) -> Result<(PathBuf, PathBuf), String> {
    let declared = Path::new(entry);
    let candidates: Vec<PathBuf> = if declared.is_absolute() {
        vec![declared.to_path_buf()]
    } else {
        vec![roots.scratch.join(declared), roots.project.join(declared)]
    };
    let found = candidates
        .iter()
        .find(|c| std::fs::symlink_metadata(c).is_ok())
        .ok_or_else(|| "no such file in the judge scratch or the project".to_string())?;
    // canonical = every symlink resolved, so the containment check sees where the bytes really are.
    let source = std::fs::canonicalize(found).map_err(|e| e.to_string())?;
    for root in [&roots.scratch, &roots.project] {
        let Ok(root) = std::fs::canonicalize(root) else { continue };
        if let Ok(rel) = source.strip_prefix(&root) {
            if rel.components().all(|c| matches!(c, Component::Normal(_))) && rel.components().next().is_some() {
                return Ok((source.clone(), rel.to_path_buf()));
            }
        }
    }
    Err("outside the judge scratch and the project — refused".into())
}

/// A judge name as a single, safe directory name. A native judge is named by a Rust string, so its
/// name may hold a `/` or be `..`; the evidence path must not climb out of `<session>/`.
pub fn dir_name(judge: &str) -> String {
    let s: String =
        judge.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        format!("_{s}")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project dir and a scratch dir, side by side, plus the destination.
    fn fixture() -> (tempfile::TempDir, Roots, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let roots = Roots { scratch: tmp.path().join("scratch"), project: tmp.path().join("proj") };
        std::fs::create_dir_all(&roots.scratch).unwrap();
        std::fs::create_dir_all(&roots.project).unwrap();
        let dest = tmp.path().join("proj/agg/private/evidence/r/1/lint");
        (tmp, roots, dest)
    }

    #[test]
    fn a_scratch_file_is_copied_under_its_scratch_relative_path() {
        let (_t, roots, dest) = fixture();
        std::fs::create_dir_all(roots.scratch.join("out")).unwrap();
        std::fs::write(roots.scratch.join("out/cc.sarif"), "{}").unwrap();
        let kept = persist(&["out/cc.sarif".into()], &roots, &dest, &mut Budget::mb(1));
        assert_eq!(kept.saved, [dest.join("out/cc.sarif")]);
        assert_eq!(std::fs::read_to_string(dest.join("out/cc.sarif")).unwrap(), "{}");
        assert!(kept.skipped.is_empty());
    }

    #[test]
    fn an_absolute_scratch_path_and_a_project_path_are_both_kept() {
        let (_t, roots, dest) = fixture();
        std::fs::write(roots.scratch.join("a.txt"), "a").unwrap();
        std::fs::write(roots.project.join("b.txt"), "b").unwrap();
        let abs = roots.scratch.join("a.txt").display().to_string();
        let kept = persist(&[abs, "b.txt".into()], &roots, &dest, &mut Budget::mb(1));
        assert_eq!(kept.saved, [dest.join("a.txt"), dest.join("b.txt")]);
    }

    /// The scratch wins over the project for a relative path — it is where a judge writes.
    #[test]
    fn a_relative_path_prefers_the_scratch() {
        let (_t, roots, dest) = fixture();
        std::fs::write(roots.scratch.join("r.txt"), "scratch").unwrap();
        std::fs::write(roots.project.join("r.txt"), "project").unwrap();
        persist(&["r.txt".into()], &roots, &dest, &mut Budget::mb(1));
        assert_eq!(std::fs::read_to_string(dest.join("r.txt")).unwrap(), "scratch");
    }

    #[test]
    fn traversal_out_of_both_roots_is_refused() {
        let (t, roots, dest) = fixture();
        std::fs::write(t.path().join("secret"), "s").unwrap();
        let abs = t.path().join("secret").display().to_string();
        let kept = persist(&["../secret".into(), abs, "/etc/hosts".into()], &roots, &dest, &mut Budget::mb(1));
        assert!(kept.saved.is_empty(), "nothing outside the roots is copied: {kept:?}");
        assert_eq!(kept.skipped.len(), 3);
        assert!(kept.skipped.iter().all(|s| s.contains("refused")), "{kept:?}");
        assert!(!dest.exists(), "a refused entry creates nothing");
    }

    #[test]
    #[cfg(unix)]
    fn a_symlink_out_of_the_roots_is_refused_and_one_inside_is_followed() {
        let (t, roots, dest) = fixture();
        std::fs::write(t.path().join("secret"), "s").unwrap();
        std::fs::write(roots.project.join("real.txt"), "r").unwrap();
        std::os::unix::fs::symlink(t.path().join("secret"), roots.scratch.join("out")).unwrap();
        std::os::unix::fs::symlink(roots.project.join("real.txt"), roots.scratch.join("in")).unwrap();
        let kept = persist(&["out".into(), "in".into()], &roots, &dest, &mut Budget::mb(1));
        assert_eq!(kept.saved, [dest.join("real.txt")], "the inside link is kept under its real path");
        assert!(kept.skipped[0].starts_with("out: ") && kept.skipped[0].contains("refused"));
    }

    #[test]
    fn a_directory_or_a_missing_file_is_skipped_with_its_reason() {
        let (_t, roots, dest) = fixture();
        std::fs::create_dir_all(roots.scratch.join("d")).unwrap();
        let kept = persist(&["d".into(), "nope".into()], &roots, &dest, &mut Budget::mb(1));
        assert!(kept.saved.is_empty());
        assert!(kept.skipped[0].contains("not a regular file"));
        assert!(kept.skipped[1].contains("no such file"));
    }

    #[test]
    fn the_cap_skips_what_does_not_fit_and_records_the_hit() {
        let (_t, roots, dest) = fixture();
        std::fs::write(roots.scratch.join("big"), vec![0u8; 600 * 1024]).unwrap();
        std::fs::write(roots.scratch.join("big2"), vec![0u8; 600 * 1024]).unwrap();
        std::fs::write(roots.scratch.join("small"), "s").unwrap();
        let mut budget = Budget::mb(1);
        let kept = persist(&["big".into(), "big2".into(), "small".into()], &roots, &dest, &mut budget);
        assert_eq!(kept.saved, [dest.join("big"), dest.join("small")], "a smaller file still fits after a refusal");
        assert!(kept.skipped[0].starts_with("big2: evidence size cap reached"), "{kept:?}");
        assert!(budget.hit);
        assert_eq!(budget.used, 600 * 1024 + 1);
    }

    #[test]
    fn two_entries_for_one_file_keep_the_first() {
        let (_t, roots, dest) = fixture();
        std::fs::write(roots.scratch.join("x"), "x").unwrap();
        let abs = roots.scratch.join("x").display().to_string();
        let kept = persist(&["x".into(), abs], &roots, &dest, &mut Budget::mb(1));
        assert_eq!(kept.saved.len(), 1);
        assert!(kept.skipped[0].contains("already saved"));
    }

    #[test]
    fn a_judge_name_becomes_one_safe_directory() {
        assert_eq!(dir_name("lint_clean"), "lint_clean");
        assert_eq!(dir_name("a/../b"), "a_.._b");
        assert_eq!(dir_name(".."), "_..");
        assert_eq!(dir_name(""), "_");
    }
}
