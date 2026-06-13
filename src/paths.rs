//! The authoritative home for AgenticGoGo's on-disk layout.
//!
//! All runtime state lives under `<project>/.agg/` (gitignored). Before this module the
//! `.agg/...` path strings were hand-joined at 8+ call sites; a single typo or a future layout
//! change had no central anchor. Every consumer now goes through these helpers, so the
//! convention is defined in exactly one place.
//!
//! ```text
//! <project>/.agg/
//!   state.json        live dashboard snapshot (loop writes, `agg dashboard` reads)
//!   project.json      persistent run-history ledger (lifetime sessions/tokens)
//!   spawns.json       long-task registry (`agg spawn`)
//!   spawns/<name>.log per-spawn combined stdout+stderr
//!   bus/{in,out}/     operator↔loop command bus; bus/log.jsonl audit
//!   run.pid           the live loop's pid (double-run guard + `agg stop` target)
//!   run.log           detached-loop log (`agg run --detach`)
//! ```

use std::path::{Path, PathBuf};

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
}
