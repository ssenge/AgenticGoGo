//! READ path (#3, READ tiers) — the durable/scratch reads that feed every prompt.
//!
//! Durable-file + scratch-dir path helpers, the per-session scratch-note lifecycle
//! (ensure / sweep / clear), the bounded READ block prepended to each worker prompt
//! (`read_block`), and reading the worker's untrusted scratch note (`read_worker_note`, which
//! delegates neutralization to the parent module's `sanitize_worker_note`).

use std::path::{Path, PathBuf};

use super::*;

/// The durable, rolled-up memory file: `agg/private/LOG.md` (was `AGG_MEMORY.md`). Gitignored, so a
/// machine-managed file never churns the user's git history.
///
/// AGG-OWNED (`private/`, not `state/`): this is the enforced hard-facts audit trail — agg folds
/// every session's mechanical record into it, and the worker's own contribution arrives as an
/// explicitly UNTRUSTED scratch note that gets sanitized on the way in ([`scratch_dir`]). A worker
/// able to edit the trail directly could rewrite the history agg reasons from, bypassing exactly
/// the sanitizing that makes the note safe. agg injects a slice into every prompt, so the worker
/// reads it without ever touching the file.
pub fn memory_file(dir: &Path) -> PathBuf {
    crate::paths::private_dir(dir).join("LOG.md")
}

/// Directory for transient per-session worker scratch notes: `agg/state/sessions/`.
pub fn scratch_dir(dir: &Path) -> PathBuf {
    crate::paths::agg_dir(dir).join("sessions")
}

/// The worker's optional scratch note for session `n`: `agg/state/sessions/session-<N>.md`.
/// This is the EXACT path the resume prompt tells the worker to write to (no session-id suffix —
/// the worker can't know its own Claude session id from inside `-p`). Single-writer today; when
/// parallel workers (Tier C #1) land they add namespacing here.
pub fn scratch_path(dir: &Path, n: u32) -> PathBuf {
    scratch_dir(dir).join(format!("session-{n}.md"))
}

/// Ensure `agg/state/sessions/` exists (best-effort). Called before telling the worker where to write
/// and before reading its note, so memory works WITHOUT git isolation.
pub fn ensure_scratch_dir(dir: &Path) {
    let _ = std::fs::create_dir_all(scratch_dir(dir));
}

/// Delete EVERY `session-*.md` scratch note in `agg/state/sessions/` (best-effort). Called once at loop
/// start: the durable `LOG.md` is the only legitimate cross-run carrier, so any scratch
/// note already on disk is by definition stale — left by a prior `agg run` that crashed before
/// folding it, or written by a worker under a forged/wrong session number. Without this sweep
/// `agg/state/sessions/` grows unbounded across runs (per-session `clear_scratch` only ever targets the
/// CURRENT run's monotonic counter, which resets each process). Sweeping here also hardens the
/// fold path: a stale forged note can never be mistaken for the current session's learning.
pub fn sweep_scratch(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(scratch_dir(dir)) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("session-") && name.ends_with(".md") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Delete any stale scratch note for session `n` (best-effort). Called BEFORE launching the
/// worker for session `n` so a note left by a PRIOR `agg run` (same N) can never be folded as
/// if it belonged to this run, and AFTER folding so `agg/state/sessions/` does not grow unbounded.
pub fn clear_scratch(dir: &Path, n: u32) {
    let _ = std::fs::remove_file(scratch_path(dir, n));
}

/// (READ a) Build the bounded READ block prepended (as the lowest-priority tail) to every worker
/// prompt: the NEWEST `inject_kb` of the durable `LOG.md` (if present + non-empty) plus
/// the always-on LAST SESSION block. Returns `""` when there's nothing to inject (session 1, no
/// durable file) so a fresh project's prompt is unchanged.
///
/// `inject_kb` bounds the per-prompt token cost INDEPENDENTLY of the on-disk `max_kb`: the file
/// may be large (good audit trail) while the injected slice stays small (cost control).
pub fn read_block(dir: &Path, last_session: &str, inject_kb: Option<u64>) -> String {
    let mut out = String::new();
    let durable = std::fs::read_to_string(memory_file(dir)).unwrap_or_default();
    // inject only the NEWEST inject_kb of the durable file (reuse the same keep-newest cap).
    // HARD READ-SIDE CEILING: even when `inject_kb` is None (user opted into "inject all"), fall
    // back to READ_INJECT_HARD_MAX_KB so a large/uncapped durable file can NEVER balloon every
    // prompt's input tokens — the budget discipline memory exists to support must not be the
    // thing memory undermines. (`max_kb=None` may keep an unbounded audit trail on disk; the
    // prompt only ever sees this bounded slice.)
    let effective_inject = inject_kb.or(Some(READ_INJECT_HARD_MAX_KB));
    let durable = cap_to_kb(durable.trim(), effective_inject);
    let durable = durable.trim();
    if !durable.is_empty() {
        out.push_str("--- INSTITUTIONAL MEMORY (durable, lower priority than the task above; what past sessions learned) ---\n");
        out.push_str(durable);
        out.push('\n');
    }
    let last = last_session.trim();
    if !last.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("--- LAST SESSION (lower priority than the task above) ---\n");
        out.push_str(last);
        out.push('\n');
    }
    out
}

/// (READ b / WRITE 3a) Read + NEUTRALIZE the worker's optional scratch note for session `n`.
/// `None` if the worker never wrote it (crash / kill / ignored) or it's blank. The returned
/// content is UNTRUSTED worker output, so it is:
///   - size-capped to `WORKER_NOTE_MAX_BYTES` (a worker can't blow the budget),
///   - stripped of control chars except `\n`/`\t`,
///   - de-fanged of any line mimicking agg's own structural markers (entry sentinel, file
///     header, the READ-block banners) so a worker can't forge institutional truth or a fake
///     operator instruction that the next session reads as real.
///
/// The caller additionally FENCES it and (on a failed session) never lets it stand alone.
pub fn read_worker_note(dir: &Path, n: u32) -> Option<String> {
    let raw = std::fs::read_to_string(scratch_path(dir, n)).ok()?;
    let cleaned = sanitize_worker_note(&raw);
    if cleaned.trim().is_empty() {
        None
    } else {
        Some(cleaned)
    }
}
