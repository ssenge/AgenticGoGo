//! WRITE path (#3, WRITE tiers) — folding learnings into the durable `LOG.md`, plus the pure
//! session-note formatters that build the bodies to fold / inject.
//!
//! Appending / superseding entries (`fold_entry` / `append_entry`), the atomic durable write,
//! cap/rotate keeping the NEWEST bytes (`cap_to_kb`), and the pure formatters that build a fold
//! body / READ block from the data the loop already holds (`last_session_block`,
//! `mechanical_note`).

use std::path::Path;

use super::*;

/// (WRITE c+d) Fold one entry into the durable `LOG.md`, then cap to `max_kb` (oldest
/// drop first). Each entry is delimited by the unique `ENTRY_SENTINEL` and headed with the
/// session + tier for human auditability. Atomic write (`.tmp` + rename). Best-effort: any error
/// is swallowed. Returns the new file size in bytes (for the dashboard), 0 on a failed write.
///
/// `supersede`: when true, a trailing entry whose header is `## session {n} (session-start)` is
/// REPLACED by this one rather than a second entry appended. This is how the early enforced fold
/// (the crash-insurance "session-start" floor) is upgraded in place by the post-judge refinement
/// — so a normally-completing session leaves exactly ONE entry, not two. When false (or there is
/// no matching trailing floor entry), this is a plain append.
pub fn fold_entry(dir: &Path, n: u32, source: &str, body: &str, max_kb: Option<u64>, supersede: bool) -> usize {
    let path = memory_file(dir);
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.trim().is_empty() {
        existing = FILE_HEADER.to_string();
    }
    // optionally drop a trailing `session-<n> (session-start)` floor entry so the refinement
    // replaces it in place (single entry per completed session).
    if supersede {
        let floor_header = format!("{ENTRY_SENTINEL}\n## session {n} (session-start)\n");
        if let Some(i) = existing.rfind(&floor_header) {
            // keep everything before that trailing floor entry.
            existing.truncate(i);
        }
    }
    let entry = format!(
        "{ENTRY_SENTINEL}\n## session {n} ({source})\n{}\n",
        body.trim()
    );
    let combined = format!("{}\n{entry}", existing.trim_end());
    let capped = cap_to_kb(&combined, max_kb);
    atomic_write(&path, &capped)
}

/// Plain append (no supersede) — kept as a thin wrapper for the early floor write + tests.
pub fn append_entry(dir: &Path, n: u32, source: &str, body: &str, max_kb: Option<u64>) -> usize {
    fold_entry(dir, n, source, body, max_kb, false)
}

/// Atomic write to `path` (`.tmp` + rename), mirroring `state.rs::write`. Returns the byte size
/// written, or 0 on failure (so a failed durable write surfaces as "no memory" on the dashboard
/// rather than a stale size).
fn atomic_write(path: &Path, content: &str) -> usize {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("md.tmp");
    if std::fs::write(&tmp, content).is_ok() && std::fs::rename(&tmp, path).is_ok() {
        content.len()
    } else {
        let _ = std::fs::remove_file(&tmp);
        0
    }
}

/// (WRITE e) Cap content to `max_kb`, keeping the NEWEST bytes (oldest learnings drop first),
/// never truncating mid-UTF-8, and never splitting an entry mid-body. `None` ⇒ no cap. On
/// truncation we re-prepend the file header + a marker so the file stays self-describing.
/// Splits ONLY on the unique `ENTRY_SENTINEL` (never markdown `---`). If the newest entry alone
/// exceeds the cap (no sentinel ahead of the cut), keeps the raw newest bytes rather than
/// dropping to marker-only — memory is never silently emptied while content exists.
pub fn cap_to_kb(content: &str, max_kb: Option<u64>) -> String {
    let kb = match max_kb {
        Some(kb) if kb > 0 => kb,
        _ => return content.to_string(),
    };
    let max = (kb as usize) * 1024;
    if content.len() <= max {
        return content.to_string();
    }
    let marker = format!("{FILE_HEADER}_(older entries dropped — capped at {kb} KB)_\n");
    let keep = max.saturating_sub(marker.len());
    if keep == 0 {
        eprintln!(
            "  [memory] WARNING: max_kb={kb} is smaller than the file header — \
             memory reduced to the cap marker only. Raise memory.max_kb."
        );
        return marker;
    }
    // keep the last `keep` bytes, UTF-8-safe.
    let tail = cap_bytes_keep_newest_chars(content, keep);
    // realign forward to the next ENTRY sentinel so we don't keep a half entry — but ONLY if one
    // exists ahead; otherwise (the newest entry alone is larger than `keep`) keep the raw tail.
    let aligned = match tail.find(ENTRY_SENTINEL) {
        Some(i) => &tail[i..],
        None => tail.as_str(),
    };
    format!("{marker}{}", aligned.trim_start())
}

/// Build the LAST SESSION block carried into the NEXT prompt's READ injection: the changed goal
/// deltas this cycle + the current scoreboard. Pure formatting over data the loop already holds —
/// no I/O. Each change line is truncated (a judge rationale can be long) and the whole block is
/// byte-capped.
pub fn last_session_block(deltas: &[crate::core::engine::GoalDelta], scoreboard: &str) -> String {
    let mut out = String::new();
    let changed = changed_lines(deltas);
    if changed.is_empty() {
        out.push_str("No goal changed last session.\n");
    } else {
        out.push_str("What moved last session:\n");
        for line in &changed {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push('\n');
    out.push_str(scoreboard.trim());
    cap_bytes_keep_newest_chars(&out, BLOCK_MAX_BYTES)
}

/// Tier-3c mechanical fallback body from data the loop already has in hand. Cannot fail / needs
/// no cooperation — guarantees the WRITE tier always has content. Used both for the early
/// (pre-judge) fold and the post-judge refinement.
#[allow(clippy::too_many_arguments)] // 8 already-computed session facts; a struct would not simplify
pub fn mechanical_note(
    exit_code: Option<i32>,
    killed_by_watchdog: bool,
    rate_limited: bool,
    duration_secs: u64,
    started_at_epoch: u64,
    ended_at_epoch: u64,
    scoreboard: &str,
    deltas: &[crate::core::engine::GoalDelta],
) -> String {
    let outcome = if killed_by_watchdog {
        "worker killed by watchdog (hung)".to_string()
    } else if rate_limited {
        "worker hit a usage/rate limit".to_string()
    } else {
        match exit_code {
            Some(0) => "worker exited cleanly".to_string(),
            Some(c) => format!("worker exited with code {c}"),
            None => "worker exit code unknown".to_string(),
        }
    };
    let mut out = format!(
        "{} → {}  ({duration_secs}s)\n{outcome}.\n",
        crate::util::human_time(started_at_epoch),
        crate::util::human_time(ended_at_epoch),
    );
    let changed = changed_lines(deltas);
    if changed.is_empty() {
        out.push_str("No goal changed this session.\n");
    } else {
        for line in &changed {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push('\n');
    out.push_str(scoreboard.trim());
    cap_bytes_keep_newest_chars(&out, BLOCK_MAX_BYTES)
}

/// Changed-goal lines, each truncated so a verbose judge rationale (embedded by
/// `GoalDelta::line()`) can't balloon the block / the prompt.
fn changed_lines(deltas: &[crate::core::engine::GoalDelta]) -> Vec<String> {
    deltas
        .iter()
        .filter(|d| d.changed())
        .map(|d| crate::util::truncate(&d.line(), LINE_MAX_CHARS))
        .collect()
}
