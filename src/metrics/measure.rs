//! ONE `swe-metrics measure` per session tree, shared by every metric and score judge of it, and
//! the INCUMBENT's report kept next to the ratchet state.
//!
//! # The cache
//! Keyed on the judge scratch (project + session), the measured scope, its `--only` families, the
//! base it is measured against and the TREE — HEAD, the diff against it, and every untracked file's
//! content. The first judge of a session measures; every later one with the same key reads the same
//! report. A failed measurement is cached too: five judges must not run a broken analyzer five
//! times. The report and the tools' details land in the session's judge scratch
//! (`$AGG_JUDGE_SCRATCH/swe-metrics/<scope>/`), which is where a verdict's evidence is taken from.
//!
//! # The incumbent
//! The report of the last KEPT state, per scope, at `agg/private/metrics/incumbent/<scope>.json`:
//! the baseline pass measures it, and every kept gate promotes the session's reports. A score judge
//! hands it to `swe-metrics score --baseline`; `combine: all` compares its tools against it.
//!
//! # Confinement
//! swe-metrics runs as a judge does (`core::judge`): from the project root, with `stdin` closed,
//! the `AGG_*` judge env, `TMPDIR` in the scratch, and under `isolation: sandbox` in the OS jail
//! with the project read-only and only the scratch and the judge cache writable. Unlike a script
//! judge, `XDG_CACHE_HOME` is left alone: swe-metrics' tools keep machine-wide caches there (trivy's
//! vulnerability database), and pointing it at a per-project directory would fetch them per project.

use super::report::Report;
use super::spec::Scope;
use crate::isolation::Isolation;
use crate::protect::sha256::{self, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// The program agg runs. Optional: agg works without it, and a metric/score judge refuses at
/// startup when it is missing (`super::probe`).
pub const PROGRAM: &str = "swe-metrics";

/// Where in the judge scratch the measurements go.
pub const SCRATCH_SUBDIR: &str = "swe-metrics";

/// How many measurements the process keeps. A run needs one per scope per session; this only bounds
/// a long-lived driver process.
const CACHE_CAP: usize = 32;

/// Everything a run of swe-metrics needs to know about the judge that asked for it.
#[derive(Debug, Clone, Copy)]
pub struct Ctx<'a> {
    pub cwd: &'a Path,
    pub session: Option<u32>,
    pub step: &'a str,
    pub judge: &'a str,
    pub timeout: u64,
    pub isolation: Isolation,
    /// the base branch `scope: changed` measures against; `None` = `HEAD`.
    pub base: Option<&'a str>,
}

/// One measurement: the parsed report and where its files are.
#[derive(Debug)]
pub struct Measured {
    pub report: Report,
    /// the measurement directory relative to the judge scratch (`swe-metrics/target`) — how a
    /// verdict names its evidence.
    pub rel: String,
    /// the same directory, absolute.
    pub dir: PathBuf,
}

impl Measured {
    /// The report file.
    pub fn report_path(&self) -> PathBuf {
        self.dir.join("report.json")
    }
}

struct Entry {
    key: String,
    scratch: PathBuf,
    scope: String,
    result: Result<Arc<Measured>, String>,
}

static CACHE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

fn cache() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    CACHE.lock().unwrap_or_else(|p| p.into_inner())
}

/// The shared measurement of `scope` for the judge in `c`: from the cache when this session's tree
/// was measured already, else measured now (holding the cache, so two judges never measure twice).
pub fn measure(c: &Ctx<'_>, scope: &Scope, only: Option<&[String]>) -> Result<Arc<Measured>, String> {
    let scratch = crate::core::judge::scratch_path(c.cwd, c.session);
    let base = c.base.unwrap_or("HEAD");
    let families = only.map(|o| o.join(",")).unwrap_or_else(|| "*".into());
    let key = format!("{}|{}|{families}|{base}|{}", scratch.display(), scope.dir_name(), tree_key(c.cwd));
    let mut entries = cache();
    if let Some(e) = entries.iter().find(|e| e.key == key) {
        return e.result.clone();
    }
    let result = run_measure(c, scope, only, base, &scratch).map(Arc::new);
    entries.push(Entry { key, scratch, scope: scope.dir_name(), result: result.clone() });
    let excess = entries.len().saturating_sub(CACHE_CAP);
    entries.drain(..excess);
    result
}

/// How many measurements this process has made (cache misses) — for tests and diagnostics.
pub fn measured_count() -> usize {
    cache().len()
}

fn run_measure(c: &Ctx<'_>, scope: &Scope, only: Option<&[String]>, base: &str, scratch: &Path) -> Result<Measured, String> {
    let rel = format!("{SCRATCH_SUBDIR}/{}", scope.dir_name());
    let dir = scratch.join(&rel);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let target = match scope {
        Scope::Path(p) if !c.cwd.join(p).exists() => return Err(format!("`scope: {p}` does not exist in the project")),
        Scope::Path(p) => c.cwd.join(p),
        _ => c.cwd.to_path_buf(),
    };
    let mut args: Vec<String> = vec!["measure".into(), target.display().to_string()];
    args.extend(["--out".into(), dir.join("report.json").display().to_string()]);
    args.extend(["--details".into(), dir.join("details").display().to_string()]);
    if *scope == Scope::Changed {
        args.extend(["--changed-since".into(), base.to_string()]);
    }
    for f in only.unwrap_or_default() {
        args.extend(["--only".into(), f.clone()]);
    }
    run_tool(c, &args).map_err(|e| format!("`swe-metrics measure` failed: {e}"))?;
    let text = std::fs::read_to_string(dir.join("report.json")).map_err(|e| format!("swe-metrics wrote no report: {e}"))?;
    let report = Report::parse(&text)?;
    Ok(Measured { report, rel, dir })
}

/// Run `swe-metrics <args>` confined as a judge; `Ok` = it exited 0 (its stdout), `Err` = why not.
pub fn run_tool(c: &Ctx<'_>, args: &[String]) -> Result<String, String> {
    let scratch = crate::core::judge::scratch_dir(c.cwd, c.session).map_err(|e| format!("judge scratch: {e}"))?;
    let cache = crate::core::judge::cache_dir(c.cwd).map_err(|e| format!("judge cache: {e}"))?;
    let mut command = Command::new(PROGRAM);
    command
        .args(args)
        .current_dir(c.cwd)
        .stdin(Stdio::null())
        .env("AGG_SESSION", c.session.map(|s| s.to_string()).unwrap_or_default())
        .env("AGG_STEP", c.step)
        .env("AGG_JUDGE", c.judge)
        .env("AGG_PROJECT_DIR", c.cwd)
        .env("AGG_JUDGE_SCRATCH", &scratch)
        .env("TMPDIR", &scratch)
        .env("PYTHONPYCACHEPREFIX", &cache);
    if c.isolation == Isolation::Sandbox {
        // the project is read-only to a judge (§2.5) — the deny of `.` wins over the cwd grant.
        command = crate::isolation::wrap(command, c.cwd, &[scratch, cache], &[".".to_string()])
            .map_err(|e| format!("could not sandbox swe-metrics: {e}"))?;
    }
    let out = crate::os::proc::run_with_timeout(command, c.timeout)?;
    if out.success {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).rev().take(3).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    Err(if tail.is_empty() { "exited non-zero (no stderr)".into() } else { tail.join(" | ") })
}

/// A key for the tree the judges see: HEAD, everything that differs from it (a staged merge
/// included), and every untracked, non-ignored file with its content. Equal keys measure equal.
fn tree_key(cwd: &Path) -> String {
    let git = |args: &[&str]| Command::new("git").args(args).current_dir(cwd).output().map(|o| o.stdout).unwrap_or_default();
    let mut h = Sha256::default();
    h.update(&git(&["rev-parse", "HEAD"]));
    h.update(&git(&["diff", "HEAD", "--binary"]));
    for f in git(&["ls-files", "-o", "--exclude-standard", "-z"]).split(|b| *b == 0).filter(|f| !f.is_empty()) {
        h.update(f);
        let p = cwd.join(String::from_utf8_lossy(f).as_ref());
        h.update(sha256::file_hex(&p).unwrap_or_default().as_bytes());
    }
    h.hex()
}

// ---------------- the incumbent ----------------

/// `agg/private/metrics/incumbent/<scope>.json` — the incumbent's report for `scope`, if kept.
pub fn incumbent(cwd: &Path, scope: &Scope) -> Option<PathBuf> {
    let p = crate::paths::metrics_incumbent_dir(cwd).join(format!("{}.json", scope.dir_name()));
    p.is_file().then_some(p)
}

/// Make this session's measurements the incumbent: the tree they measured is now the kept state.
/// Every scope measured in `session`'s scratch by THIS process replaces its incumbent, and a scope
/// it did not measure is dropped (it describes an older tree). Returns what could not be kept.
pub fn promote(cwd: &Path, session: Option<u32>) -> Vec<String> {
    let scratch = crate::core::judge::scratch_path(cwd, session);
    let latest: Vec<(String, PathBuf)> = {
        let entries = cache();
        let mut seen: Vec<(String, PathBuf)> = Vec::new();
        for e in entries.iter().rev().filter(|e| e.scratch == scratch) {
            if let (Ok(m), false) = (&e.result, seen.iter().any(|(s, _)| s == &e.scope)) {
                seen.push((e.scope.clone(), m.report_path()));
            }
        }
        seen
    };
    if latest.is_empty() {
        return vec![];
    }
    let dir = crate::paths::metrics_incumbent_dir(cwd);
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return vec![format!("creating {}: {e}", dir.display())];
    }
    latest
        .into_iter()
        .filter_map(|(scope, report)| std::fs::copy(&report, dir.join(format!("{scope}.json"))).err().map(|e| format!("{scope}: {e}")))
        .collect()
}

/// Forget the incumbent — a fresh run whose first measurement has not been kept yet must not read
/// a previous run's.
pub fn forget_incumbent(cwd: &Path) {
    let _ = std::fs::remove_dir_all(crate::paths::metrics_incumbent_dir(cwd));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key moves with every change the judges could see — HEAD, a tracked edit, an untracked
    /// file's content — and stays put when nothing moved.
    #[test]
    fn the_tree_key_follows_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let git = |a: &[&str]| assert!(Command::new("git").args(a).current_dir(d).output().unwrap().status.success(), "git {a:?}");
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(d.join("a.py"), "x = 1\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "base"]);
        let k0 = tree_key(d);
        assert_eq!(tree_key(d), k0, "nothing moved");
        std::fs::write(d.join("a.py"), "x = 2\n").unwrap();
        let k1 = tree_key(d);
        assert_ne!(k1, k0, "a tracked edit");
        std::fs::write(d.join("new.py"), "y = 1\n").unwrap();
        let k2 = tree_key(d);
        assert_ne!(k2, k1, "an untracked file");
        std::fs::write(d.join("new.py"), "y = 2\n").unwrap();
        assert_ne!(tree_key(d), k2, "an untracked file's CONTENT");
    }

    /// No incumbent until one is promoted; forgetting it removes it.
    #[test]
    fn the_incumbent_is_absent_until_kept() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(incumbent(tmp.path(), &Scope::Target).is_none());
        assert!(promote(tmp.path(), Some(7)).is_empty(), "nothing measured, nothing promoted");
        let dir = crate::paths::metrics_incumbent_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("target.json"), "{}").unwrap();
        assert!(incumbent(tmp.path(), &Scope::Target).is_some());
        forget_incumbent(tmp.path());
        assert!(incumbent(tmp.path(), &Scope::Target).is_none());
    }
}
