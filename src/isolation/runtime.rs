//! The container tier as an AGENT RUNTIME — how each kind of process is re-hosted in a container:
//! which image it runs in, which `HOME` it gets, and which credentials cross into it.
//!
//! | process | image | project | `HOME` (under `agg/private/container/home/`) | credentials |
//! |---|---|---|---|---|
//! | worker | the step's `image:` | read-write (minus the carve-outs) | `worker`, FRESH every session | its agent's |
//! | LLM judge (one-shot) | the judges' image | read-only | `ruler` | the ruler's |
//! | script judge | the judges' image | read-only | `judge` | none (only `pass_env`) |
//! | swe-metrics (a metric/score judge's measurement) | the judges' image | read-only | `metrics`, FRESH every call | none (only `pass_env`) |
//!
//! Every one gets the run's `isolation_policy.pass_env` and `credentials`, its context dirs, and
//! runs as the project owner (see [`super::container::containerize_spec`]).
//!
//! # Why the worker's home is fresh every session
//! Loop Engineering: a fresh session each cycle. An agent's home holds its settings, hooks, memory
//! and transcripts; a home that survives the session is a channel from one worker session into the
//! next that agg neither sees nor controls. The judges' homes are separate and read-only to the
//! worker (they sit in `agg/private/`), so a worker can never plant a setting its grader then runs
//! with.
//!
//! # Why swe-metrics' home is fresh every call
//! swe-metrics runs the project's own tool configs (a dependency-cruiser config is JavaScript), so a
//! measurement of a worker's tree may run worker-authored code. A home that survived it — or any
//! other writable dir that outlives the session — would carry that code's leftovers into the next
//! session's measurement and score. Its only writes are its session scratch and this fresh home.

use super::container::{containerize_spec, Spec};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Re-host one WORKER session: the step's `image`, the project read-write minus `denies`, a fresh
/// home, and its agent's credentials (`env_names`, `credential_files`).
pub fn worker(cmd: Command, dir: &Path, denies: &[String], image: &str, env_names: &[&str], credential_files: &[&str]) -> Result<Command> {
    let home = fresh_home(dir, "worker")?;
    containerize_spec(cmd, dir, &Spec { image, denies, home: Some(&home), env_names, credential_files, ..Spec::default() })
}

/// Re-host an LLM judge's one-shot in the judges' image: the project READ-ONLY (a grader never
/// changes what it grades), the ruler's credentials.
pub fn one_shot(cmd: Command, dir: &Path, env_names: &[&str], credential_files: &[&str]) -> Result<Command> {
    let image = super::policy::for_dir(dir).judge_image.clone();
    let home = home(dir, "ruler")?;
    let spec = Spec { image: &image, project_ro: true, home: Some(&home), env_names, credential_files, ..Spec::default() };
    containerize_spec(cmd, dir, &spec)
}

/// Re-host a SCRIPT judge in the judges' image: the project read-only, `writable` (its scratch and
/// toolchain cache, both under [`crate::paths::container_dir`]) read-write, and the judge file's own
/// directory read-only when it lives outside the project (the standard library in `~/.agg/judges`).
pub fn script_judge(cmd: Command, dir: &Path, script: &Path, writable: &[PathBuf]) -> Result<Command> {
    let image = super::policy::for_dir(dir).judge_image.clone();
    let home = home(dir, "judge")?;
    let project = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let lib: Vec<PathBuf> = script.parent().filter(|p| !p.starts_with(&project)).map(Path::to_path_buf).into_iter().collect();
    let spec = Spec { image: &image, writable, readonly: &lib, project_ro: true, home: Some(&home), ..Spec::default() };
    containerize_spec(cmd, dir, &spec)
}

/// Re-host `swe-metrics` — the measurement behind every metric and score judge — in the judges'
/// image: the project read-only, `writable` (its session scratch) read-write, `readonly` (the
/// host's trivy cache, a scoring definition kept outside the project) read-only, and a FRESH home.
pub fn metrics_judge(cmd: Command, dir: &Path, writable: &[PathBuf], readonly: &[PathBuf]) -> Result<Command> {
    let image = super::policy::for_dir(dir).judge_image.clone();
    let home = fresh_home(dir, "metrics")?;
    let spec = Spec { image: &image, writable, readonly, project_ro: true, home: Some(&home), ..Spec::default() };
    containerize_spec(cmd, dir, &spec)
}

/// A judge's per-session scratch when it runs in a container — what `$AGG_JUDGE_SCRATCH` and
/// `$TMPDIR` point at, and what evidence resolves against ([`crate::core::judge::scratch_path`]).
pub fn judge_scratch(dir: &Path, session: Option<u32>) -> PathBuf {
    crate::paths::container_dir(dir).join("judge").join(format!("scratch-{}", session.unwrap_or(0)))
}

/// A containerized judge's per-project toolchain cache (`CARGO_TARGET_DIR`, `GOCACHE`, …).
pub fn judge_cache(dir: &Path) -> PathBuf {
    crate::paths::container_dir(dir).join("judge").join("cache")
}

/// The home `label` gets: `agg/private/container/home/<label>`, created when missing. Canonical,
/// because it is a bind source.
fn home(dir: &Path, label: &str) -> Result<PathBuf> {
    let p = crate::paths::container_dir(dir).join("home").join(label);
    std::fs::create_dir_all(&p).with_context(|| format!("creating the container home {}", p.display()))?;
    Ok(std::fs::canonicalize(&p).unwrap_or(p))
}

/// A NEW, empty home for `label` — `home/<label>-<unique>` — after removing every earlier one.
///
/// A new PATH, not the old one emptied: a VM-backed engine (colima's virtiofs) caches directory
/// entries, and a home deleted and recreated under the same name was seen, measured, still holding
/// the previous session's file in the next container — then vanishing under it mid-session.
fn fresh_home(dir: &Path, label: &str) -> Result<PathBuf> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = crate::paths::container_dir(dir).join("home");
    let prefix = format!("{label}-");
    for old in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        if old.file_name().to_string_lossy().starts_with(&prefix) {
            std::fs::remove_dir_all(old.path()).with_context(|| format!("clearing the container home {}", old.path().display()))?;
        }
    }
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    home(dir, &format!("{prefix}{}-{n}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    /// Each worker session gets a NEW empty home, and the earlier ones are gone; a judge's home
    /// persists.
    #[test]
    fn the_worker_home_is_fresh_every_session_and_the_judges_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let w1 = fresh_home(tmp.path(), "worker").unwrap();
        std::fs::write(w1.join("planted"), "x").unwrap();
        let w2 = fresh_home(tmp.path(), "worker").unwrap();
        assert_ne!(w1, w2, "a new path, not the old one emptied");
        assert!(!w1.exists(), "the earlier home is removed");
        assert_eq!(std::fs::read_dir(&w2).unwrap().count(), 0, "a fresh home starts empty");
        let j = home(tmp.path(), "judge").unwrap();
        std::fs::write(j.join("cache"), "x").unwrap();
        assert!(home(tmp.path(), "judge").unwrap().join("cache").exists());
        assert!(w2.starts_with(std::fs::canonicalize(crate::paths::container_dir(tmp.path())).unwrap()));
    }

    /// A worker: project read-write, its fresh home mounted and exported, the agent's credentials
    /// crossing by NAME only — the value is never on the argv.
    #[test]
    fn a_worker_gets_a_home_and_its_credentials_by_name_never_by_value() {
        let tmp = tempfile::tempdir().unwrap();
        let key = "AGG_TEST_RUNTIME_SECRET_KEY";
        std::env::set_var(key, "s3cr3t-value");
        let cmd = worker(Command::new("claude"), tmp.path(), &[], "img:1", &[key, "AGG_TEST_RUNTIME_UNSET"], &[]).unwrap();
        let args = argv(&cmd);
        let canon = std::fs::canonicalize(tmp.path()).unwrap();
        assert!(args.contains(&format!("{p}:{p}", p = canon.display())), "project read-write: {args:?}");
        let home = args.iter().find_map(|a| a.strip_prefix("HOME=")).expect("HOME exported").to_string();
        assert!(home.starts_with(&canon.join("agg/private/container/home/worker-").display().to_string()), "{home}");
        assert!(args.contains(&format!("{home}:{home}")), "home mounted: {args:?}");
        assert!(args.windows(2).any(|w| w[0] == "-e" && w[1] == key), "passed by name: {args:?}");
        assert!(!args.iter().any(|a| a.contains("s3cr3t-value")), "the VALUE never reaches the argv: {args:?}");
        assert!(!args.iter().any(|a| a.contains("AGG_TEST_RUNTIME_UNSET")), "an unset name is not passed: {args:?}");
        assert!(args.windows(2).any(|w| w[0] == "--user"), "runs as the project owner: {args:?}");
        std::env::remove_var(key);
    }

    /// A judge never writes the tree it grades: the project is read-only, its scratch writable.
    #[test]
    fn a_script_judge_reads_the_project_and_writes_only_its_scratch() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tempfile::tempdir().unwrap();
        let scratch = judge_scratch(tmp.path(), Some(3));
        std::fs::create_dir_all(&scratch).unwrap();
        let script = lib.path().join("cargo_test");
        let cmd = script_judge(Command::new(&script), tmp.path(), &script, std::slice::from_ref(&scratch)).unwrap();
        let args = argv(&cmd);
        let canon = std::fs::canonicalize(tmp.path()).unwrap();
        assert!(args.contains(&format!("{p}:{p}:ro", p = canon.display())), "project read-only: {args:?}");
        let s = std::fs::canonicalize(&scratch).unwrap();
        assert!(args.contains(&format!("{p}:{p}", p = s.display())), "scratch writable: {args:?}");
        let l = std::fs::canonicalize(lib.path()).unwrap();
        assert!(args.contains(&format!("{p}:{p}:ro", p = l.display())), "a library judge's dir, read-only: {args:?}");
        assert!(scratch.ends_with("agg/private/container/judge/scratch-3"));
    }
}
