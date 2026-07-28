//! The authoritative home for AgenticGoGo's on-disk layout.
//!
//! All runtime state lives under `<project>/agg/` (gitignored), split into TWO directories by
//! **who is allowed to write them**. Before this module the path strings were hand-joined at 8+
//! call sites; a single typo or a future layout change had no central anchor. Every consumer now
//! goes through these helpers, so the convention is defined in exactly one place.
//!
//! ```text
//! <project>/
//!   agg/                  user config — COMMITTED (agg.yaml, AGG.md, judges/<name>.{sh,md})
//!     AGG.md            standing project instructions the WORKER reads (the CLAUDE.md-analog) — COMMITTED
//!     state/              WORKER-WRITABLE runtime state — GITIGNORED
//!       STATE.md            worker-curated forward advice (survives rollback)
//!       wiki/               worker-owned durable knowledge — multi-session PLANS + dead-ends
//!       sessions/           transient per-session worker memory scratch (session-<N>.md)
//!       spawns.json         long-task registry — `agg spawn` is invoked BY THE WORKER
//!       spawns/<name>.log   per-spawn combined stdout+stderr
//!       BLOCKED.md          the worker's self-report, if the project authors a `blocked` judge
//!     private/            AGG-OWNED runtime state — GITIGNORED, and the worker CANNOT WRITE IT
//!       INSTRUCTIONS.md     the worker's whole `-p` target — agg REGENERATES it every session
//!       LOG.md              durable institutional memory (#3) — enforced hard-facts audit trail
//!       state.json          live dashboard snapshot (loop writes, `agg dashboard` reads)
//!       project.json        persistent run-history ledger (lifetime sessions/tokens)
//!       verdicts.jsonl      append-only, safety-critical GATE record — one line per verdict (§5.8)
//!       bus/{in,out}/       operator↔loop command bus; bus/log.jsonl audit
//!       run.pid             the live loop's pid (double-run guard + `agg stop` target)
//!       run.log             detached-loop log (`agg run --detach`)
//! ```
//!
//! # Why the split exists
//! Everything used to sit in one folder under one rule, and that rule was wrong: `agg/state/` is
//! inside the worker's cwd, so a confined worker could write **every** file in it. Three of them
//! decide when the loop ends or what it costs:
//!
//! - `verdicts.jsonl` — the ledger `stalled`/`stuck` read. Appending three forged `"merged"` rows
//!   makes `stalled` report met, and a project with `abort_if: "stalled"` ends its own run. Proven
//!   in review under `isolation: sandbox`.
//! - `bus/` — `agg send` steering. A worker writing here raises its own token ceiling
//!   (`agg send budget …`), unpauses itself, or injects its own next-session instructions.
//! - `run.pid` — the double-run guard and `agg stop` target.
//!
//! Splitting them into `private/` lets [`crate::isolation`] carve that ONE subpath out of the
//! worker's writable set. Reads stay open: the worker still reads its brief, and a judge still
//! reads the ledger. See `internal/ISOLATION.md`.
//!
//! **This only binds under `isolation: sandbox`/`container`.** Under the default `none` the worker
//! has the whole filesystem and no layout can change that — which is what the other tiers are for.

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

/// The WORKER-WRITABLE runtime-state root: `<dir>/agg/state`. Gitignored. The worker curates its
/// own files here (forward advice, wiki, scratch notes, spawns); agg reads them as untrusted input.
pub fn agg_dir(dir: &Path) -> PathBuf {
    config_base(dir).join("state")
}

/// The AGG-OWNED runtime-state root: `<dir>/agg/private`. Gitignored, and carved OUT of the
/// worker's writable set by [`crate::isolation`] under `sandbox`/`container`.
///
/// The rule for deciding where a new file goes: **if the worker writing it could change when the
/// loop ends, what it may spend, or what agg believes happened, it belongs here.** Everything the
/// worker is supposed to author belongs in [`agg_dir`].
pub fn private_dir(dir: &Path) -> PathBuf {
    config_base(dir).join("private")
}

/// The worker's whole `-p` target: `agg/private/INSTRUCTIONS.md`. agg REGENERATES it fresh every
/// session (compose_prompt writes it); the worker READS it and follows it. Disposable, gitignored.
///
/// Private, though the worker reads it every session: it is the worker's ORDERS. A worker able to
/// rewrite its own brief mid-run could launder instructions past the operator. Reads are untouched
/// by the carve-out.
pub fn instructions_md(dir: &Path) -> PathBuf {
    private_dir(dir).join("INSTRUCTIONS.md")
}

/// The worker-owned LLM wiki root: `agg/state/wiki/` (durable knowledge incl. multi-session plans,
/// gitignored). WORKER-WRITABLE — this is the worker's own knowledge base, by design.
pub fn wiki_dir(dir: &Path) -> PathBuf {
    agg_dir(dir).join("wiki")
}

/// Live dashboard snapshot: `agg/private/state.json`. agg publishes it; every reader (TUI, `agg
/// status`, `agg serve`) trusts it, so the worker must not be able to forge a scoreboard.
pub fn state_json(dir: &Path) -> PathBuf {
    private_dir(dir).join("state.json")
}

/// Persistent run-history ledger: `agg/private/project.json` (lifetime sessions/tokens).
pub fn project_json(dir: &Path) -> PathBuf {
    private_dir(dir).join("project.json")
}

/// Append-only, safety-critical GATE record (§5.8): `agg/private/verdicts.jsonl`.
///
/// THE file this split exists for. `stalled`/`stuck` derive "is the loop making progress" from it,
/// and a project may wire that to `abort_if` — so a worker able to append forged rows can end its
/// own run, which is the one thing agg exists to prevent.
pub fn verdicts_jsonl(dir: &Path) -> PathBuf {
    private_dir(dir).join("verdicts.jsonl")
}

/// Long-task registry: `agg/state/spawns.json`. WORKER-WRITABLE — `agg spawn` is documented as
/// "used by the worker, not to start the loop", so confining this would break the feature. Its
/// blast radius is bounded: it records what to reap and what to tell the next session, nothing
/// that gates the run.
pub fn spawns_json(dir: &Path) -> PathBuf {
    agg_dir(dir).join("spawns.json")
}

/// Directory of per-spawn logs: `agg/state/spawns/`. WORKER-WRITABLE, with `spawns.json`.
pub fn spawns_log_dir(dir: &Path) -> PathBuf {
    agg_dir(dir).join("spawns")
}

/// The command-bus root: `agg/private/bus/`.
///
/// Private because the bus is the OPERATOR's channel (`agg send inject|budget|pause|resume`). A
/// worker writing here would raise its own token ceiling, unpause itself, or inject its own
/// next-session instructions — steering the loop it is supposed to be steered by.
pub fn bus_dir(dir: &Path) -> PathBuf {
    private_dir(dir).join("bus")
}

/// The live loop's pidfile: `agg/private/run.pid` (double-run guard + `agg stop` target).
pub fn run_pid(dir: &Path) -> PathBuf {
    private_dir(dir).join("run.pid")
}

/// The detached-loop log: `agg/private/run.log` (`agg run --detach`).
pub fn run_log(dir: &Path) -> PathBuf {
    private_dir(dir).join("run.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The SPLIT, asserted as one table. Every path is on the side that matches who writes it —
    /// getting one wrong either breaks the worker (a file it must write, confined) or silently
    /// reopens the hole (a file agg trusts, left writable). This test IS the classification.
    #[test]
    fn runtime_state_is_split_by_who_may_write_it() {
        let d = Path::new("/proj");
        assert_eq!(agg_dir(d), Path::new("/proj/agg/state"));
        assert_eq!(private_dir(d), Path::new("/proj/agg/private"));

        // WORKER-WRITABLE — the worker is supposed to author these.
        assert_eq!(wiki_dir(d), Path::new("/proj/agg/state/wiki"));
        assert_eq!(spawns_json(d), Path::new("/proj/agg/state/spawns.json"));
        assert_eq!(spawns_log_dir(d), Path::new("/proj/agg/state/spawns"));

        // AGG-OWNED — the worker writing any of these could change when the loop ends, what it may
        // spend, or what agg believes happened.
        assert_eq!(verdicts_jsonl(d), Path::new("/proj/agg/private/verdicts.jsonl"));
        assert_eq!(bus_dir(d), Path::new("/proj/agg/private/bus"));
        assert_eq!(state_json(d), Path::new("/proj/agg/private/state.json"));
        assert_eq!(project_json(d), Path::new("/proj/agg/private/project.json"));
        assert_eq!(instructions_md(d), Path::new("/proj/agg/private/INSTRUCTIONS.md"));
        assert_eq!(run_pid(d), Path::new("/proj/agg/private/run.pid"));
        assert_eq!(run_log(d), Path::new("/proj/agg/private/run.log"));

        // and the carve-out must actually CONTAIN the private files, or the sandbox deny (which is
        // one subpath rule) would confine nothing.
        for p in [verdicts_jsonl(d), bus_dir(d), state_json(d), run_pid(d)] {
            assert!(p.starts_with(private_dir(d)), "{} must live under the carve-out", p.display());
        }
    }

    #[test]
    fn config_lives_in_the_agg_folder() {
        // no dual layout any more: config always resolves inside `agg/`, never the project root.
        let d = Path::new("/proj");
        assert_eq!(config_base(d), Path::new("/proj/agg"));
    }
}
