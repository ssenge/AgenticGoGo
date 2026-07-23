//! Institutional memory (#3) — ENFORCED, agg-owned learnings that survive across sessions.
//!
//! Four layers, all driven by plain loop code (the worker is NEVER trusted to persist):
//!   READ  (every prompt, pure code — bounded so it never blows the token budget):
//!     1. the NEWEST `inject_kb` of the durable file `agg/state/LOG.md`. Capped on the
//!        READ side independently of the on-disk cap, so a large durable file does not balloon
//!        every prompt's input tokens.
//!     2. an always-on "LAST SESSION" block carried in a loop-local String (the prior cycle's
//!        deltas + scoreboard); empty on session 1 of an invocation.
//!   WRITE (3-tier — first that yields content wins; runs even on crash/kill):
//!     3a. a worker-written scratch note `agg/state/sessions/session-<N>.md` → SANITIZED, size-capped,
//!         fenced, and on a NON-clean session never allowed to stand alone (the failure fact is
//!         always recorded). Deleted after reading and before each launch so a stale note from a
//!         prior run can never be folded.
//!     3b. else the windowed summary, ONLY when one was freshly computed THIS cycle (NO extra
//!         LLM call; never the stale persistent value).
//!     3c. else mechanical facts (exit/scoreboard/deltas) — needs no I/O, cannot fail to produce
//!         content. This is the enforcement floor.
//!   then cap/rotate `LOG.md` to `max_kb`, dropping the OLDEST entries first.
//!
//! All I/O is best-effort: a disk error degrades memory, it never breaks the loop. The durable
//! file is written atomically (`.tmp` + rename), mirroring `state.rs`, so an interrupted write
//! can never leave a torn file. The durable file and per-session worker scratch both live under
//! the gitignored `agg/state/` (`LOG.md` and `sessions/session-<N>.md` respectively).
//!
//! SINGLE-WRITER ASSUMPTION: `LOG.md` is mutated by exactly one loop. Parallel workers
//! (Tier C #1) are not yet supported; when they land, that work adds the append locking. Until
//! then atomic-rename prevents a torn file; a hypothetical race is last-writer-wins.
//!
//! This module is split by responsibility:
//!
//! - `read` — durable/scratch path helpers, scratch-note lifecycle, the bounded READ injection
//!   block, and reading the worker's untrusted scratch note.
//! - `write` — folding/superseding entries into `LOG.md`, the atomic write, cap/rotate, and the
//!   pure formatters that build a fold body / READ block.
//!
//! The tuning constants and the two pure, security-critical primitives every tier leans on —
//! UTF-8-safe byte capping and untrusted worker-note neutralization — live here in the parent.

mod read;
mod write;

pub use read::*;
pub use write::*;

#[cfg(test)]
mod tests;

/// Unique entry separator. An HTML comment so it can never collide with free-text markdown a
/// worker note or summary might contain (`---`, code fences, YAML front-matter). The cap logic
/// splits on THIS, never on `---`.
const ENTRY_SENTINEL: &str = "<!--agg-entry-->";
/// Title line of the durable file (written once, on first append; re-prepended on truncation).
const FILE_HEADER: &str =
    "# LOG.md — institutional memory (agg-managed; oldest entries drop when capped)\n";
/// Hard cap on a single worker note (Tier 3a) before folding — a worker cannot blow the budget
/// or push out all real memory in one shot.
const WORKER_NOTE_MAX_BYTES: usize = 8 * 1024;
/// Cap on any single change-line / mechanical/last-session block (defends against a verbose
/// judge rationale, which `GoalDelta::line()` embeds verbatim).
const LINE_MAX_CHARS: usize = 200;
const BLOCK_MAX_BYTES: usize = 4 * 1024;
/// Hard ceiling on the durable slice injected into each prompt when `inject_kb` is `None`
/// ("inject all"). A safety floor so a misconfigured uncapped file can't blow the token budget
/// memory exists to protect — the prompt never sees more than this, regardless of config.
const READ_INJECT_HARD_MAX_KB: u64 = 32;

/// True if `line` mimics one of agg's own structural markers — an entry boundary, the file
/// header, a READ-block banner, the operator-instruction banner (BOTH the `═` glyph form and a
/// near-miss ASCII `===`/text form), or a forged `## session N (...)` entry header identical to
/// what `append_entry` emits. Matching by TEXT (not just the leading glyph) closes the near-miss
/// holes: `=== HIGH-PRIORITY OPERATOR INSTRUCTION ===`, a bare `HIGH-PRIORITY OPERATOR
/// INSTRUCTION:` line, and `## session 99 (worker)` must all be neutralized.
fn looks_like_marker(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with(ENTRY_SENTINEL)
        || t.starts_with("# LOG.md")
        || t.starts_with("# AGG_MEMORY.md") // keep de-fanging the pre-rename header a worker might forge
        || t.starts_with("--- INSTITUTIONAL MEMORY")
        || t.starts_with("--- LAST SESSION")
        || t.starts_with('\u{2550}') // ═
        || t.starts_with('=') // ASCII near-miss of the ═ banner rule
    {
        return true;
    }
    // operator-instruction banner by TEXT, however it's decorated.
    if t.to_ascii_uppercase().contains("HIGH-PRIORITY OPERATOR INSTRUCTION") {
        return true;
    }
    // forged entry header: `## session <digits> (` — exactly the shape append_entry writes.
    if let Some(rest) = t.strip_prefix("## session ") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() && rest[digits.len()..].trim_start().starts_with('(') {
            return true;
        }
    }
    false
}

/// Neutralize untrusted worker-note text (see `read_worker_note`). Pure/total — easy to test.
/// Beyond stripping control chars + de-fanging marker lines, this also neutralizes ```` ``` ````
/// fence lines so a worker can't break OUT of the code fence the caller wraps the note in.
fn sanitize_worker_note(raw: &str) -> String {
    // 1) size cap (bytes), UTF-8-safe.
    let capped = cap_bytes_keep_newest_chars(raw, WORKER_NOTE_MAX_BYTES);
    // 2) strip control chars except \n and \t; 3) de-fang structural markers + fence breakouts.
    capped
        .lines()
        .map(|line| {
            let cleaned: String = line
                .chars()
                .filter(|c| *c == '\t' || !c.is_control())
                .collect();
            // neutralize a code-fence breakout (``` or more backticks at line start).
            if cleaned.trim_start().starts_with("```") {
                return cleaned.replacen("```", "'''", 1);
            }
            if looks_like_marker(&cleaned) {
                // prefix so a forged marker becomes inert text, never a real boundary/banner.
                format!("> {cleaned}")
            } else {
                cleaned
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Keep the newest `max` BYTES of `s`, backing up to a UTF-8 char boundary so we never split a
/// multibyte sequence. Returns `s` unchanged when already within `max`.
fn cap_bytes_keep_newest_chars(s: &str, max: usize) -> String {
    let bytes = s.as_bytes();
    if bytes.len() <= max {
        return s.to_string();
    }
    let mut cut = bytes.len() - max;
    while cut < bytes.len() && !s.is_char_boundary(cut) {
        cut += 1;
    }
    s[cut..].to_string()
}
