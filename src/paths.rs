//! The authoritative home for Relentless's on-disk layout.
//!
//! All runtime state lives under `<project>/rlt/` (gitignored), split into TWO directories by
//! **who is allowed to write them**. Before this module the path strings were hand-joined at 8+
//! call sites; a single typo or a future layout change had no central anchor. Every consumer now
//! goes through these helpers, so the convention is defined in exactly one place.
//!
//! ```text
//! <project>/
//!   rlt/                  user config — COMMITTED (rlt.yaml, RLT.md, verifiers/<name>.{sh,md})
//!     RLT.md            standing project instructions the WORKER reads (the CLAUDE.md-analog) — COMMITTED
//!     state/              WORKER-WRITABLE runtime state — GITIGNORED
//!       STATE.md            worker-curated forward advice (survives rollback)
//!       wiki/               worker-owned durable knowledge — multi-session PLANS + dead-ends
//!       sessions/           transient per-session worker memory scratch (session-<N>.md)
//!       spawns.json         long-task registry — `rlt spawn` is invoked BY THE WORKER
//!       spawns/<name>.log   per-spawn combined stdout+stderr
//!       BLOCKED.md          the worker's self-report, if the project authors a `blocked` verifier
//!     private/            RLT-OWNED runtime state — GITIGNORED, and the worker CANNOT WRITE IT
//!       INSTRUCTIONS.md     the worker's whole `-p` target — rlt REGENERATES it every session
//!       LOG.md              durable institutional memory (#3) — enforced hard-facts audit trail
//!       state.json          live dashboard snapshot (loop writes, `rlt dashboard` reads)
//!       project.json        persistent run-history ledger (lifetime sessions/tokens)
//!       verdicts.jsonl      append-only, safety-critical GATE record — one line per verdict (§5.8)
//!       gate.json           the gate's own state: ratchet references, rollback/oscillation history
//!       bus/{in,out}/       operator↔loop command bus; bus/log.jsonl audit
//!       run.pid             the live loop's pid (double-run guard + `rlt stop` target)
//!       run.log             the run log: run ids, `--detach`/`--report-json` output
//!       report.json         the end-of-run report — the stable interface for embedders
//!       violations.jsonl    append-only record of `protected_path_violation` rollbacks (write_deny)
//!       waits.jsonl         append-only record of every rate-limit wait: its start and how it ended
//!       protected/<sha256>  rlt's own copies of every `write_deny` file, to restore a violation from
//!       evidence/<run>/<session>/<verifier>/  the files each verdict cited, kept after its verifier ran
//!       prompts.jsonl       per session, the hash of every prompt file its brief was read from
//!       metrics/incumbent/<scope>.json  the swe-metrics report of the last KEPT state (a score's baseline)
//!       container/          `isolation: container` runtime: worker + verifier homes, verifier scratch
//! ```
//!
//! # Why the split exists
//! Everything used to sit in one folder under one rule, and that rule was wrong: `rlt/state/` is
//! inside the worker's cwd, so a confined worker could write **every** file in it. Three of them
//! decide when the loop ends or what it costs:
//!
//! - `verdicts.jsonl` — the ledger `stalled`/`stuck` read. Appending three forged `"merged"` rows
//!   makes `stalled` report met, and a project with `abort_if: "stalled"` ends its own run. Proven
//!   in review under `isolation: sandbox`.
//! - `bus/` — `rlt send` steering. A worker writing here raises its own token ceiling
//!   (`rlt send budget …`), unpauses itself, or injects its own next-session instructions.
//! - `run.pid` — the double-run guard and `rlt stop` target.
//!
//! Splitting them into `private/` lets [`crate::isolation`] carve that ONE subpath out of the
//! worker's writable set. Reads stay open: the worker still reads its brief, and a verifier still
//! reads the ledger. See `internal/ISOLATION.md`.
//!
//! **This only binds under `isolation: sandbox`/`container`.** Under the default `none` the worker
//! has the whole filesystem and no layout can change that — which is what the other tiers are for.

use std::path::{Path, PathBuf};

/// The MANDATORY config-dir name. User inputs (rlt.yaml, the state file, the verifiers under verifiers/)
/// live under `<project>/rlt/`, and all runtime state under `<project>/rlt/state/`.
/// (Runtime state living inside `rlt/` is what makes the folder mandatory.)
pub const CONFIG_DIR: &str = "rlt";

/// Where user-provided config lives for `dir`: `<dir>/rlt/`. This is the base that `rlt.yaml`,
/// the state file, and `.md` verifier rubrics resolve against. (Verifier *scripts* still run
/// from the project root, so a verifier's `cmd`/`inputs` reference real project files — only
/// config-adjacent files live here.)
pub fn config_base(dir: &Path) -> PathBuf {
    dir.join(CONFIG_DIR)
}

/// The WORKER-WRITABLE runtime-state root: `<dir>/rlt/state`. Gitignored. The worker curates its
/// own files here (forward advice, wiki, scratch notes, spawns); rlt reads them as untrusted input.
pub fn rlt_dir(dir: &Path) -> PathBuf {
    config_base(dir).join("state")
}

/// The RLT-OWNED runtime-state root: `<dir>/rlt/private`. Gitignored, and carved OUT of the
/// worker's writable set by [`crate::isolation`] under `sandbox`/`container`.
///
/// The rule for deciding where a new file goes: **if the worker writing it could change when the
/// loop ends, what it may spend, or what rlt believes happened, it belongs here.** Everything the
/// worker is supposed to author belongs in [`rlt_dir`].
pub fn private_dir(dir: &Path) -> PathBuf {
    config_base(dir).join("private")
}

/// The worker's whole `-p` target: `rlt/private/INSTRUCTIONS.md`. rlt REGENERATES it fresh every
/// session (compose_prompt writes it); the worker READS it and follows it. Disposable, gitignored.
///
/// Private, though the worker reads it every session: it is the worker's ORDERS. A worker able to
/// rewrite its own brief mid-run could launder instructions past the operator. Reads are untouched
/// by the carve-out.
pub fn instructions_md(dir: &Path) -> PathBuf {
    private_dir(dir).join("INSTRUCTIONS.md")
}

/// The worker-owned LLM wiki root: `rlt/state/wiki/` (durable knowledge incl. multi-session plans,
/// gitignored). WORKER-WRITABLE — this is the worker's own knowledge base, by design.
pub fn wiki_dir(dir: &Path) -> PathBuf {
    rlt_dir(dir).join("wiki")
}

/// Live dashboard snapshot: `rlt/private/state.json`. rlt publishes it; every reader (TUI, `rlt
/// status`, `rlt serve`) trusts it, so the worker must not be able to forge a scoreboard.
pub fn state_json(dir: &Path) -> PathBuf {
    private_dir(dir).join("state.json")
}

/// Persistent run-history ledger: `rlt/private/project.json` (lifetime sessions/tokens).
pub fn project_json(dir: &Path) -> PathBuf {
    private_dir(dir).join("project.json")
}

/// Append-only, safety-critical GATE record (§5.8): `rlt/private/verdicts.jsonl`.
///
/// THE file this split exists for. `stalled`/`stuck` derive "is the loop making progress" from it,
/// and a project may wire that to `abort_if` — so a worker able to append forged rows can end its
/// own run, which is the one thing rlt exists to prevent.
pub fn verdicts_jsonl(dir: &Path) -> PathBuf {
    private_dir(dir).join("verdicts.jsonl")
}

/// Append-only DRIVER CALL ledger: `rlt/private/calls.jsonl` (`core::calls`).
///
/// A sibling of [`verdicts_jsonl`] and private for a strictly stronger reason. A worker able to
/// append rows here could make rlt **skip work and fabricate verdicts** on the next `--resume`,
/// because that is precisely what a ledger hit does: it answers a call without executing it. The
/// rule this file states — *if the worker writing it could change when the loop ends, what it may
/// spend, or what rlt believes happened, it belongs here* — covers all three at once.
pub fn calls_jsonl(dir: &Path) -> PathBuf {
    private_dir(dir).join("calls.jsonl")
}

/// The gate's own state: `rlt/private/gate.json` (`core::ratchet`) — the ratchet references and the
/// rollback / oscillation history behind `ratchet_regressed`, `<verifier>.ref`, `rollbacks`,
/// `consecutive_rollbacks` and `oscillating`.
///
/// Private because those terms may sit in `abort_if`: a worker able to rewrite this file could lower
/// its own ratchet references or zero its rollback count, which is exactly "change when the loop ends".
pub fn gate_json(dir: &Path) -> PathBuf {
    private_dir(dir).join("gate.json")
}

/// The append-only human-ask ledger: `<dir>/rlt/private/asks.jsonl`.
///
/// RLT-OWNED, like `verdicts.jsonl` and for the same reason: an ask's *request* may be
/// worker-authored, but its **answer** must not be. The answer arrives on the operator bus, which
/// is carved out of the worker's writable set under a confining tier, so a worker cannot approve
/// its own deploy. See `internal/HUMAN_LOOP.md` §4.1.
pub fn asks_jsonl(dir: &Path) -> PathBuf {
    private_dir(dir).join("asks.jsonl")
}

/// Where the WORKER drops ask requests: `<dir>/rlt/state/asks/`.
///
/// Worker-writable on purpose — `rlt hil` runs inside the worker's own confinement, so it cannot
/// write `private/`. rlt promotes these into [`asks_jsonl`] at the next session boundary. The
/// request is untrusted either way; only the answer's channel decides trust.
pub fn worker_asks_dir(dir: &Path) -> PathBuf {
    rlt_dir(dir).join("asks")
}

/// The run clock: `<dir>/rlt/private/clock.json`.
///
/// Holds the run's `started_at_epoch` and accumulated `human_wait_secs`. It exists because
/// `wall_time` is defined as END-TO-END (`internal/HUMAN_LOOP.md` §7.4) and a resumed run is a new
/// process: an `Instant` restarts and would hand the run a fresh allowance every time a human
/// answered. Epoch-on-disk is what makes the number mean what its name says.
pub fn clock_json(dir: &Path) -> PathBuf {
    private_dir(dir).join("clock.json")
}

/// Long-task registry: `rlt/state/spawns.json`. WORKER-WRITABLE — `rlt spawn` is documented as
/// "used by the worker, not to start the loop", so confining this would break the feature. Its
/// blast radius is bounded: it records what to reap and what to tell the next session, nothing
/// that gates the run.
pub fn spawns_json(dir: &Path) -> PathBuf {
    rlt_dir(dir).join("spawns.json")
}

/// Directory of per-spawn logs: `rlt/state/spawns/`. WORKER-WRITABLE, with `spawns.json`.
pub fn spawns_log_dir(dir: &Path) -> PathBuf {
    rlt_dir(dir).join("spawns")
}

/// The command-bus root: `rlt/private/bus/`.
///
/// Private because the bus is the OPERATOR's channel (`rlt send inject|budget|pause|resume`). A
/// worker writing here would raise its own token ceiling, unpause itself, or inject its own
/// next-session instructions — steering the loop it is supposed to be steered by.
pub fn bus_dir(dir: &Path) -> PathBuf {
    private_dir(dir).join("bus")
}

/// The `write_deny` violation ledger: `rlt/private/violations.jsonl` ([`crate::protect`]).
///
/// Private because it is what rlt believes happened: a worker able to delete a row would erase the
/// record that its session was rolled back for changing an oracle.
pub fn violations_jsonl(dir: &Path) -> PathBuf {
    private_dir(dir).join("violations.jsonl")
}

/// The wait ledger: `rlt/private/waits.jsonl` — one row when a rate-limit wait starts, one when it
/// ends and how (A-18.1).
///
/// Private because it is what rlt believes happened: it is the record that a stopped run was waiting
/// on a provider, not working, and where the interrupted session's work was saved.
pub fn waits_jsonl(dir: &Path) -> PathBuf {
    private_dir(dir).join("waits.jsonl")
}

/// The content-addressed copies of every protected file: `rlt/private/protected/<sha256>`.
///
/// Private so a confined worker cannot plant the bytes a restore would write back. Under
/// `isolation: none` it can, which is why a restore checks every copy against the hash rlt holds
/// in memory before using it.
pub fn protected_store(dir: &Path) -> PathBuf {
    private_dir(dir).join("protected")
}

/// The container runtime's own dirs: `rlt/private/container/` — a fresh `HOME` for each worker
/// session, the verifiers' homes, and the scratch + toolchain cache of a verifier that runs in a
/// container ([`crate::isolation::runtime`]).
///
/// Private for two reasons. The worker must not plant anything a VERIFIER then runs with (a
/// `~/.claude/settings.json` hook in the ruler's home would steer its grader), and must not write a
/// verifier's measurements. And it is under the PROJECT on purpose rather than in `$TMPDIR`: the engine
/// can always see the project (it is bind-mounted), while a VM-backed engine (colima, lima) often
/// cannot see `$TMPDIR` — and a bind of a path the engine cannot see is not an error, it is an EMPTY
/// directory inside the container.
pub fn container_dir(dir: &Path) -> PathBuf {
    private_dir(dir).join("container")
}

/// The kept verifier evidence: `rlt/private/evidence/<run>/<session>/<verifier>/` (`core::evidence`).
///
/// Private because it is what a verdict CITES: a worker able to write here could swap the SARIF
/// file a passing gate points at for one that tells a different story, after the fact.
pub fn evidence_dir(dir: &Path) -> PathBuf {
    private_dir(dir).join("evidence")
}

/// The prompt-file ledger: `rlt/private/prompts.jsonl` (`core::prompts`) — per session, the hash
/// of every `prompt_file` / `role_prompt_file` its brief was composed from.
///
/// Private because it is the audit trail of what the worker was TOLD: a worker able to rewrite it
/// could hide which version of its orders a session ran under.
pub fn prompts_jsonl(dir: &Path) -> PathBuf {
    private_dir(dir).join("prompts.jsonl")
}

/// The incumbent's swe-metrics reports: `rlt/private/metrics/incumbent/<scope>.json`
/// (`metrics::measure`) — the report of the last kept state, kept next to the ratchet state.
///
/// Private because a score verifier scores AGAINST it (`--baseline`): a worker able to swap it could
/// hand its own session a flattering baseline, so that a metric that got worse costs no points.
pub fn metrics_incumbent_dir(dir: &Path) -> PathBuf {
    private_dir(dir).join("metrics").join("incumbent")
}

/// The live loop's pidfile: `rlt/private/run.pid` (double-run guard + `rlt stop` target).
pub fn run_pid(dir: &Path) -> PathBuf {
    private_dir(dir).join("run.pid")
}

/// The run log: `rlt/private/run.log`. Every run stamps its `run_id` here; `--detach` and
/// `--report-json` send the whole process output here too.
pub fn run_log(dir: &Path) -> PathBuf {
    private_dir(dir).join("run.log")
}

/// The end-of-run report: `rlt/private/report.json` (`crate::report`).
///
/// Private because it is what an embedding program ACTS on — a worker able to write it could turn
/// an aborted run into a `done` one for whoever reads it.
pub fn report_json(dir: &Path) -> PathBuf {
    private_dir(dir).join("report.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The SPLIT, asserted as one table. Every path is on the side that matches who writes it —
    /// getting one wrong either breaks the worker (a file it must write, confined) or silently
    /// reopens the hole (a file rlt trusts, left writable). This test IS the classification.
    #[test]
    fn runtime_state_is_split_by_who_may_write_it() {
        let d = Path::new("/proj");
        assert_eq!(rlt_dir(d), Path::new("/proj/rlt/state"));
        assert_eq!(private_dir(d), Path::new("/proj/rlt/private"));

        // WORKER-WRITABLE — the worker is supposed to author these.
        assert_eq!(wiki_dir(d), Path::new("/proj/rlt/state/wiki"));
        assert_eq!(spawns_json(d), Path::new("/proj/rlt/state/spawns.json"));
        assert_eq!(spawns_log_dir(d), Path::new("/proj/rlt/state/spawns"));

        // RLT-OWNED — the worker writing any of these could change when the loop ends, what it may
        // spend, or what rlt believes happened.
        assert_eq!(verdicts_jsonl(d), Path::new("/proj/rlt/private/verdicts.jsonl"));
        // a ledger HIT skips work and returns a recorded verdict — a worker able to append here
        // could make rlt fabricate both. Strictly stronger than the verdicts.jsonl case.
        assert_eq!(calls_jsonl(d), Path::new("/proj/rlt/private/calls.jsonl"));
        // the ratchet references and the rollback count back `abort_if` terms.
        assert_eq!(gate_json(d), Path::new("/proj/rlt/private/gate.json"));
        assert_eq!(bus_dir(d), Path::new("/proj/rlt/private/bus"));
        assert_eq!(state_json(d), Path::new("/proj/rlt/private/state.json"));
        assert_eq!(project_json(d), Path::new("/proj/rlt/private/project.json"));
        assert_eq!(instructions_md(d), Path::new("/proj/rlt/private/INSTRUCTIONS.md"));
        assert_eq!(run_pid(d), Path::new("/proj/rlt/private/run.pid"));
        // what a verdict cites — swapping a kept file rewrites why a gate passed.
        assert_eq!(evidence_dir(d), Path::new("/proj/rlt/private/evidence"));
        // the record of which prompt version each session was given.
        assert_eq!(prompts_jsonl(d), Path::new("/proj/rlt/private/prompts.jsonl"));
        assert_eq!(run_log(d), Path::new("/proj/rlt/private/run.log"));
        // what an embedder ACTS on — a forged `done` would ship unfinished work.
        assert_eq!(report_json(d), Path::new("/proj/rlt/private/report.json"));
        // a worker deleting a row erases the record of its own rollback; planting a copy would
        // make the restore write the worker's bytes back over the oracle.
        assert_eq!(violations_jsonl(d), Path::new("/proj/rlt/private/violations.jsonl"));
        assert_eq!(protected_store(d), Path::new("/proj/rlt/private/protected"));
        // a worker deleting a row would hide that a stop came while the run was waiting.
        assert_eq!(waits_jsonl(d), Path::new("/proj/rlt/private/waits.jsonl"));
        // a score verifier's baseline — a swapped one would make a worse metric cost nothing.
        assert_eq!(metrics_incumbent_dir(d), Path::new("/proj/rlt/private/metrics/incumbent"));
        // the verifiers' homes + scratch in a container: a worker writing here steers its grader.
        assert_eq!(container_dir(d), Path::new("/proj/rlt/private/container"));

        // and the carve-out must actually CONTAIN the private files, or the sandbox deny (which is
        // one subpath rule) would confine nothing.
        for p in [verdicts_jsonl(d), calls_jsonl(d), gate_json(d), bus_dir(d), state_json(d), run_pid(d), report_json(d),
                  violations_jsonl(d), protected_store(d), waits_jsonl(d), metrics_incumbent_dir(d), container_dir(d)] {
            assert!(p.starts_with(private_dir(d)), "{} must live under the carve-out", p.display());
        }
    }

    #[test]
    fn config_lives_in_the_rlt_folder() {
        // no dual layout any more: config always resolves inside `rlt/`, never the project root.
        let d = Path::new("/proj");
        assert_eq!(config_base(d), Path::new("/proj/rlt"));
    }
}
