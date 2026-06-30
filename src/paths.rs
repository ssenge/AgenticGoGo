//! The authoritative home for AgenticGoGo's on-disk layout.
//!
//! All runtime state lives under `<project>/.agg/` (gitignored). Before this module the
//! `.agg/...` path strings were hand-joined at 8+ call sites; a single typo or a future layout
//! change had no central anchor. Every consumer now goes through these helpers, so the
//! convention is defined in exactly one place.
//!
//! ```text
//! <project>/
//!   AGG_MEMORY.md       durable institutional memory (#3) — committable, user-visible (NOT in .agg/)
//!   .agg/
//!     state.json        live dashboard snapshot (loop writes, `agg dashboard` reads)
//!     project.json      persistent run-history ledger (lifetime sessions/tokens)
//!     memory/           transient per-session worker memory scratch (session-<N>.md)
//!     spawns.json       long-task registry (`agg spawn`)
//!     spawns/<name>.log per-spawn combined stdout+stderr
//!     bus/{in,out}/     operator↔loop command bus; bus/log.jsonl audit
//!     run.pid           the live loop's pid (double-run guard + `agg stop` target)
//!     run.log           detached-loop log (`agg run --detach`)
//! ```

use std::path::{Path, PathBuf};

/// The optional config-dir name. If `<project>/agg/` exists, user inputs (agg.yaml,
/// goals.yaml, the resume prompt, judges/, rubrics/) live there; otherwise they live in the
/// project root. Runtime state always lives in `.agg/` regardless.
pub const CONFIG_DIR: &str = "agg";

/// Where user-provided config lives for `dir`: `<dir>/agg/` if that directory exists, else
/// `<dir>` itself. This is the base that `agg.yaml`, `goals.yaml`, the resume prompt, and
/// rubric files resolve against. (Judge *commands* still run from the project root, so a goal's
/// `cmd`/`inputs` reference real project files — only config-adjacent files move.)
pub fn config_base(dir: &Path) -> PathBuf {
    let folded = dir.join(CONFIG_DIR);
    if folded.is_dir() {
        folded
    } else {
        dir.to_path_buf()
    }
}

/// Resolve a named config file (`agg.yaml` / `goals.yaml`) under `dir`, honouring the optional
/// `agg/` folder. Returns the path inside `agg/` if that file exists there, else the root path
/// (which is also the correct path to report as "missing" when neither exists).
pub fn config_file(dir: &Path, name: &str) -> PathBuf {
    let folded = dir.join(CONFIG_DIR).join(name);
    if folded.is_file() {
        folded
    } else {
        dir.join(name)
    }
}

/// The runtime-state root: `<dir>/.agg`.
pub fn agg_dir(dir: &Path) -> PathBuf {
    dir.join(".agg")
}

/// Live dashboard snapshot: `.agg/state.json`.
pub fn state_json(dir: &Path) -> PathBuf {
    agg_dir(dir).join("state.json")
}

/// Persistent run-history ledger: `.agg/project.json`.
pub fn project_json(dir: &Path) -> PathBuf {
    agg_dir(dir).join("project.json")
}

/// Long-task registry: `.agg/spawns.json`.
pub fn spawns_json(dir: &Path) -> PathBuf {
    agg_dir(dir).join("spawns.json")
}

/// Directory of per-spawn logs: `.agg/spawns/`.
pub fn spawns_log_dir(dir: &Path) -> PathBuf {
    agg_dir(dir).join("spawns")
}

/// The command-bus root: `.agg/bus/`.
pub fn bus_dir(dir: &Path) -> PathBuf {
    agg_dir(dir).join("bus")
}

/// The live loop's pidfile: `.agg/run.pid`.
pub fn run_pid(dir: &Path) -> PathBuf {
    agg_dir(dir).join("run.pid")
}

/// The detached-loop log: `.agg/run.log`.
pub fn run_log(dir: &Path) -> PathBuf {
    agg_dir(dir).join("run.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn layout_is_under_dot_agg() {
        let d = Path::new("/proj");
        assert_eq!(agg_dir(d), Path::new("/proj/.agg"));
        assert_eq!(state_json(d), Path::new("/proj/.agg/state.json"));
        assert_eq!(project_json(d), Path::new("/proj/.agg/project.json"));
        assert_eq!(spawns_json(d), Path::new("/proj/.agg/spawns.json"));
        assert_eq!(spawns_log_dir(d), Path::new("/proj/.agg/spawns"));
        assert_eq!(bus_dir(d), Path::new("/proj/.agg/bus"));
        assert_eq!(run_pid(d), Path::new("/proj/.agg/run.pid"));
        assert_eq!(run_log(d), Path::new("/proj/.agg/run.log"));
    }

    fn tmpdir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "agg-paths-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn config_base_is_root_without_agg_dir() {
        let d = tmpdir("noagg");
        assert_eq!(config_base(&d), d);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn config_base_prefers_agg_dir_when_present() {
        let d = tmpdir("withagg");
        std::fs::create_dir_all(d.join(CONFIG_DIR)).unwrap();
        assert_eq!(config_base(&d), d.join(CONFIG_DIR));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn config_file_falls_back_to_root() {
        let d = tmpdir("fileroot");
        std::fs::write(d.join("agg.yaml"), "x").unwrap();
        // no agg/ dir → root path
        assert_eq!(config_file(&d, "agg.yaml"), d.join("agg.yaml"));
        // neither exists → still the root path (so "missing" reports the root location)
        assert_eq!(config_file(&d, "goals.yaml"), d.join("goals.yaml"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn config_file_prefers_agg_dir_when_file_there() {
        let d = tmpdir("filefolded");
        std::fs::create_dir_all(d.join(CONFIG_DIR)).unwrap();
        std::fs::write(d.join(CONFIG_DIR).join("goals.yaml"), "x").unwrap();
        assert_eq!(config_file(&d, "goals.yaml"), d.join(CONFIG_DIR).join("goals.yaml"));
        // a file only in root still resolves to root even when agg/ exists
        std::fs::write(d.join("agg.yaml"), "x").unwrap();
        assert_eq!(config_file(&d, "agg.yaml"), d.join("agg.yaml"));
        std::fs::remove_dir_all(&d).ok();
    }
}
