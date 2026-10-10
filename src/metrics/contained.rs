//! swe-metrics on the CONTAINER tier: the measurement behind every metric and score judge runs in
//! the judges' image, like every other judge of a container run (`isolation::runtime`).
//!
//! | | |
//! |---|---|
//! | image | `judge.image` |
//! | project | read-only — the incumbent's report under `agg/private/metrics/` is read from there |
//! | writes | the session's judge scratch (`agg/private/container/judge/scratch-<n>/`), a FRESH home, the container's own `/tmp` (`TMPDIR`) |
//! | network, context dirs, `pass_env` | the run's, exactly as a script judge gets them |
//! | trivy's database | the host's, READ-ONLY, never updated in there ([`trivy_env`]) |
//!
//! # Why nothing persists between measurements
//! swe-metrics runs the measured project's own tool configs, so a measurement of a worker's tree can
//! run worker-authored code. Whatever it can write and a later measurement reads is a channel into
//! the next session's grade — the per-project toolchain cache a script judge keeps, a home, a
//! bytecode cache (`PYTHONPYCACHEPREFIX` would hold swe-metrics' own compiled modules). So it gets
//! none of them: its session scratch, a fresh home, and `PYTHONDONTWRITEBYTECODE`.
//!
//! # Why `TMPDIR` is the container's `/tmp`, not the scratch
//! Tools put unix sockets there (tflint talks to its ruleset plugin over one), and a socket path is
//! limited to ~108 bytes: under a project's `agg/private/container/judge/scratch-<n>/` it overflows
//! and the tool fails as if it were not installed. The container's `/tmp` is as private and is gone
//! with the container; the report and the details still go to the scratch (`--out`, `--details`).
//!
//! # Why trivy's database is the host's, read-only
//! It is ~1.5 GB and refreshed daily. Per project it would be downloaded per project; in the fresh
//! home, every measurement; in a cache shared by every project's judges, one project's measurement
//! could rewrite the database another project's vulnerability gate trusts. The host's own cache —
//! which the `sandbox` tier reads too — is mounted read-only with updates off, so the container needs
//! no network for it. No database there: trivy fails at once, and every judge reading
//! `dependencies.*` (a score's vulnerability gate included) fails closed, saying so.

use super::measure::{Ctx, PROGRAM};
use std::path::{Path, PathBuf};

/// `swe-metrics <args>` in the judges' image, as the judge in `c`. `readonly` = host paths outside
/// the project the call reads (a scoring definition's directory).
pub fn command(c: &Ctx<'_>, args: &[String], readonly: &[PathBuf]) -> Result<std::process::Command, String> {
    let scratch = crate::isolation::runtime::judge_scratch(c.cwd, c.session);
    std::fs::create_dir_all(&scratch).map_err(|e| format!("judge scratch {}: {e}", scratch.display()))?;
    let mut command = super::measure::command(c, args, &scratch);
    command.env("TMPDIR", "/tmp").env("PYTHONDONTWRITEBYTECODE", "1");
    let trivy = trivy_cache();
    for (k, v) in trivy_env(trivy.as_deref()) {
        command.env(k, v);
    }
    let mounts: Vec<PathBuf> = readonly.iter().cloned().chain(trivy).collect();
    crate::isolation::runtime::metrics_judge(command, c.cwd, &[scratch], &mounts)
        .map_err(|e| format!("could not containerize {PROGRAM}: {e:#}"))
}

/// trivy's settings in the container: the host's database at `cache` (mounted read-only) and never
/// an update — a database it cannot write, and no download behind the run's network policy. The scan
/// cache stays in memory, as the database dir is read-only.
pub fn trivy_env(cache: Option<&Path>) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("TRIVY_SKIP_DB_UPDATE", "true".to_string()),
        ("TRIVY_SKIP_JAVA_DB_UPDATE", "true".to_string()),
        ("TRIVY_CACHE_BACKEND", "memory".to_string()),
    ];
    if let Some(c) = cache {
        env.push(("TRIVY_CACHE_DIR", c.display().to_string()));
    }
    env
}

/// The host's trivy cache when it holds a vulnerability database — canonical, as it is a bind
/// source.
pub fn trivy_cache() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = trivy_dir(var("TRIVY_CACHE_DIR"), var("XDG_CACHE_HOME"), var("HOME"), cfg!(target_os = "macos"))?;
    dir.join("db").join("trivy.db").is_file().then(|| std::fs::canonicalize(&dir).unwrap_or(dir))
}

/// Where trivy on the host keeps its cache: `TRIVY_CACHE_DIR`, else Go's user cache dir + `trivy`
/// (`~/Library/Caches` on macOS; `$XDG_CACHE_HOME` when absolute, else `~/.cache`, elsewhere).
fn trivy_dir(explicit: Option<PathBuf>, xdg: Option<PathBuf>, home: Option<PathBuf>, macos: bool) -> Option<PathBuf> {
    if explicit.is_some() {
        return explicit;
    }
    let base = match (macos, xdg) {
        (true, _) => home?.join("Library/Caches"),
        (false, Some(x)) if x.is_absolute() => x,
        (false, _) => home?.join(".cache"),
    };
    Some(base.join("trivy"))
}

/// Where the host's trivy database is expected — for messages.
pub fn trivy_expected() -> String {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    trivy_dir(var("TRIVY_CACHE_DIR"), var("XDG_CACHE_HOME"), var("HOME"), cfg!(target_os = "macos"))
        .map(|d| d.join("db").display().to_string())
        .unwrap_or_else(|| "trivy's cache dir".into())
}

/// The directory of `file` when it lies outside `project` — what a container must mount to read it.
pub fn outside_project(project: &Path, file: &Path) -> Option<PathBuf> {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let dir = canon(file.parent()?);
    let inside = dir.starts_with(canon(project)) || file.starts_with(project);
    (!inside).then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TRIVY_CACHE_DIR` wins; macOS keeps caches in `~/Library/Caches` whatever XDG says; elsewhere
    /// an absolute `XDG_CACHE_HOME`, else `~/.cache`.
    #[test]
    fn the_host_trivy_cache_is_where_trivy_puts_it() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(trivy_dir(p("/t"), p("/x"), p("/h"), true), p("/t"));
        assert_eq!(trivy_dir(None, p("/x"), p("/h"), true), p("/h/Library/Caches/trivy"));
        assert_eq!(trivy_dir(None, p("/x"), p("/h"), false), p("/x/trivy"));
        assert_eq!(trivy_dir(None, p("rel"), p("/h"), false), p("/h/.cache/trivy"), "a relative XDG_CACHE_HOME is ignored");
        assert_eq!(trivy_dir(None, None, None, false), None);
    }

    /// Updates are always off — with no database trivy fails at once instead of downloading 1.5 GB
    /// into a home that is gone after the call.
    #[test]
    fn trivy_never_updates_in_the_container() {
        let none = trivy_env(None);
        assert!(none.contains(&("TRIVY_SKIP_DB_UPDATE", "true".into())) && none.iter().all(|(k, _)| *k != "TRIVY_CACHE_DIR"));
        let some = trivy_env(Some(Path::new("/c/trivy")));
        assert!(some.contains(&("TRIVY_CACHE_DIR", "/c/trivy".into())) && some.contains(&("TRIVY_CACHE_BACKEND", "memory".into())));
    }

    #[test]
    fn only_a_file_outside_the_project_needs_a_mount() {
        let proj = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        assert_eq!(outside_project(proj.path(), &proj.path().join("agg/scoring.py")), None);
        let outside = outside_project(proj.path(), &other.path().join("scoring.py")).unwrap();
        assert_eq!(outside, std::fs::canonicalize(other.path()).unwrap());
    }

    /// The argv: the judges' image, the project read-only, the session scratch writable, a fresh home
    /// under `agg/private/container/home/`, and the inner command intact at the end.
    #[test]
    fn swe_metrics_runs_in_the_judges_image_on_a_read_only_project() {
        let tmp = tempfile::tempdir().unwrap();
        let d = std::fs::canonicalize(tmp.path()).unwrap();
        let c = Ctx { cwd: &d, session: Some(4), step: "s", judge: "j", timeout: 5, isolation: crate::isolation::Isolation::Container, base: None };
        let cmd = command(&c, &["schema".into(), "report".into()], &[]).unwrap();
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&format!("{p}:{p}:ro", p = d.display())), "project read-only: {args:?}");
        let scratch = d.join("agg/private/container/judge/scratch-4");
        assert!(args.contains(&format!("{p}:{p}", p = scratch.display())), "scratch writable: {args:?}");
        assert!(args.contains(&format!("AGG_JUDGE_SCRATCH={}", scratch.display())), "{args:?}");
        assert!(args.contains(&"TMPDIR=/tmp".to_string()) && !args.contains(&format!("TMPDIR={}", scratch.display())), "{args:?}");
        assert!(args.contains(&"PYTHONDONTWRITEBYTECODE=1".to_string()) && args.contains(&"TRIVY_SKIP_DB_UPDATE=true".to_string()), "{args:?}");
        let home = args.iter().find_map(|a| a.strip_prefix("HOME=")).expect("HOME").to_string();
        assert!(home.starts_with(&d.join("agg/private/container/home/metrics-").display().to_string()), "{home}");
        assert!(!args.iter().any(|a| a.contains("PYTHONPYCACHEPREFIX") || a.contains("judge/cache")), "no persistent cache: {args:?}");
        assert_eq!(args[args.len() - 4..], [crate::isolation::DEFAULT_IMAGE, PROGRAM, "schema", "report"]);
    }
}
