//! The authoritative home for AgenticGoGo's on-disk layout.
//!
//! All runtime state lives under `<project>/agg/state/` (gitignored). Before this module the
//! path strings were hand-joined at 8+ call sites; a single typo or a future layout
//! change had no central anchor. Every consumer now goes through these helpers, so the
//! convention is defined in exactly one place.
//!
//! ```text
//! <project>/
//!   agg/                  user config — COMMITTED (agg.yaml, AGG.md, judges/<name>.{sh,md})
//!     AGG.md            standing project instructions the WORKER reads (the CLAUDE.md-analog) — COMMITTED
//!     state/              ALL runtime state — GITIGNORED (one folder, one rule)
//!       INSTRUCTIONS.md     the worker's whole `-p` target — agg REGENERATES it every session
//!       STATE.md            worker-curated forward advice (moved out of committed agg/, survives rollback)
//!       LOG.md              durable institutional memory (#3) — enforced hard-facts audit trail
//!       wiki/               worker-owned durable knowledge — multi-session PLANS + dead-ends (linked md pages)
//!       state.json          live dashboard snapshot (loop writes, `agg dashboard` reads)
//!       project.json        persistent run-history ledger (lifetime sessions/tokens)
//!       verdicts.jsonl      append-only, safety-critical GATE record — one line per verdict (§5.8)
//!       sessions/           transient per-session worker memory scratch (session-<N>.md)
//!       spawns.json         long-task registry (`agg spawn`)
//!       spawns/<name>.log   per-spawn combined stdout+stderr
//!       bus/{in,out}/       operator↔loop command bus; bus/log.jsonl audit
//!       run.pid             the live loop's pid (double-run guard + `agg stop` target)
//!       run.log             detached-loop log (`agg run --detach`)
//! ```

use std::path::{Path, PathBuf};

/// The MANDATORY config-dir name. User inputs (agg.yaml, the state file, the judges under judges/)
/// live under `<project>/agg/`, and all runtime state under `<project>/agg/state/`.
/// (Runtime state living inside `agg/` is what makes the folder mandatory.)
pub const CONFIG_DIR: &str = "agg";

/// Where user-provided config lives for `dir`: `<dir>/agg/`. This is the base that `agg.yaml`,
/// the state file, and `.md` judge rubrics resolve against. (Judge *scripts* still run
/// from the project root, so a judge's `cmd`/`inputs` reference real project files — only
/// config-adjacent files live here.)
pub fn config_base(dir: &Path) -> PathBuf {
    dir.join(CONFIG_DIR)
}

/// The runtime-state root: `<dir>/agg/state`. Everything agg writes lives under here, gitignored.
pub fn agg_dir(dir: &Path) -> PathBuf {
    config_base(dir).join("state")
}

/// The worker's whole `-p` target: `agg/state/INSTRUCTIONS.md`. agg REGENERATES it fresh every
/// session (compose_prompt writes it); the worker reads it and follows it. Disposable, gitignored.
pub fn instructions_md(dir: &Path) -> PathBuf {
    agg_dir(dir).join("INSTRUCTIONS.md")
}

/// The worker-owned LLM wiki root: `agg/state/wiki/` (durable knowledge incl. multi-session plans, gitignored).
pub fn wiki_dir(dir: &Path) -> PathBuf {
    agg_dir(dir).join("wiki")
}

/// Live dashboard snapshot: `agg/state/state.json`.
pub fn state_json(dir: &Path) -> PathBuf {
    agg_dir(dir).join("state.json")
}

/// Persistent run-history ledger: `agg/state/project.json`.
pub fn project_json(dir: &Path) -> PathBuf {
    agg_dir(dir).join("project.json")
}

/// Append-only, safety-critical GATE record (§5.8): `agg/state/verdicts.jsonl`.
pub fn verdicts_jsonl(dir: &Path) -> PathBuf {
    agg_dir(dir).join("verdicts.jsonl")
}

/// Long-task registry: `agg/state/spawns.json`.
pub fn spawns_json(dir: &Path) -> PathBuf {
    agg_dir(dir).join("spawns.json")
}

/// Directory of per-spawn logs: `agg/state/spawns/`.
pub fn spawns_log_dir(dir: &Path) -> PathBuf {
    agg_dir(dir).join("spawns")
}

/// The command-bus root: `agg/state/bus/`.
pub fn bus_dir(dir: &Path) -> PathBuf {
    agg_dir(dir).join("bus")
}

/// The live loop's pidfile: `agg/state/run.pid`.
pub fn run_pid(dir: &Path) -> PathBuf {
    agg_dir(dir).join("run.pid")
}

/// The detached-loop log: `agg/state/run.log`.
pub fn run_log(dir: &Path) -> PathBuf {
    agg_dir(dir).join("run.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn runtime_state_is_under_agg_state() {
        let d = Path::new("/proj");
        assert_eq!(agg_dir(d), Path::new("/proj/agg/state"));
        assert_eq!(instructions_md(d), Path::new("/proj/agg/state/INSTRUCTIONS.md"));
        assert_eq!(wiki_dir(d), Path::new("/proj/agg/state/wiki"));
        assert_eq!(state_json(d), Path::new("/proj/agg/state/state.json"));
        assert_eq!(project_json(d), Path::new("/proj/agg/state/project.json"));
        assert_eq!(verdicts_jsonl(d), Path::new("/proj/agg/state/verdicts.jsonl"));
        assert_eq!(spawns_json(d), Path::new("/proj/agg/state/spawns.json"));
        assert_eq!(spawns_log_dir(d), Path::new("/proj/agg/state/spawns"));
        assert_eq!(bus_dir(d), Path::new("/proj/agg/state/bus"));
        assert_eq!(run_pid(d), Path::new("/proj/agg/state/run.pid"));
        assert_eq!(run_log(d), Path::new("/proj/agg/state/run.log"));
    }

    #[test]
    fn config_lives_in_the_agg_folder() {
        // no dual layout any more: config always resolves inside `agg/`, never the project root.
        let d = Path::new("/proj");
        assert_eq!(config_base(d), Path::new("/proj/agg"));
    }
}
