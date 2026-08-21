//! Small pure helpers shared across modules. Previously copy-pasted: `now_epoch` lived in
//! four files, `truncate` in three (one copy had already silently diverged), and
//! `last_json_object` was byte-identical in judge.rs and summary.rs. One home, one definition.

/// Seconds since the Unix epoch (UTC). 0 if the clock is somehow before the epoch.
/// Write `content` to `path` via a sibling tmp file + `rename(2)`, so a reader never sees a
/// half-written file. The same write-then-rename the bus, the ledgers and `state.json` already do
/// by hand; this is the shared spelling for new call sites.
pub fn write_atomic(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A Unix epoch as a human-readable LOCAL wall-clock string (`YYYY-MM-DD HH:MM:SS`), for logs a
/// person reads (e.g. the `LOG.md` session header). Falls back to the raw epoch if the
/// clock is unrepresentable.
pub fn human_time(epoch: u64) -> String {
    jiff::Timestamp::from_second(epoch as i64)
        .map(|ts| {
            ts.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| format!("epoch {epoch}"))
}

/// Read + parse a YAML config file, naming the file in BOTH failure modes (can't read it vs.
/// can't parse it) — the distinction a user needs to fix the problem. Used for every config the
/// loop refuses to start without: agg.yaml.
///
/// Contrast [`load_json_or_default`]: config errors are fatal and must be loud; runtime state is
/// best-effort and must never fail the loop.
pub fn load_yaml<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> anyhow::Result<T> {
    use anyhow::Context;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Read + parse a JSON state file, falling back to `T::default()` if it is missing, unreadable,
/// or corrupt. Used for the runtime bookkeeping the loop must survive without: the run ledger,
/// the spawn registry.
///
/// The tolerance is deliberate, not laziness: a torn or hand-edited `project.json` must not stop
/// a run — it degrades to an empty ledger. See [`load_yaml`] for the fail-loud counterpart.
pub fn load_json_or_default<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Compact duration for the display surfaces: `3h12m` · `5m03s` · `45s`.
///
/// Was copy-pasted three times (dashboard, status, project) and — as copies do — had already
/// diverged: all three agreed above an hour, but below one, status printed `5m` (and a bare
/// `0m` for anything under a minute, which read as "no uptime" on a freshly started loop) while
/// dashboard printed `0m45s`. This is project.rs's variant, the strict superset of the three.
///
/// Not `humantime::format_duration`: it renders `1h 2m` / `5m 5s`, and reshaping that into the
/// TUI's compact form costs more code than the eight lines below.
pub fn fmt_dur(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Compact byte size for the memory indicator: `1.2 KB` · `640 B`. Was byte-identical in
/// dashboard.rs and status.rs.
///
/// Not `bytesize`: it renders binary units as `1.2 KiB` and rolls over to `MB`/`GB`, so adopting
/// it would change what the TUI and `agg status` print today.
pub fn human_bytes(n: usize) -> String {
    if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// Truncate `s` to at most `n` characters (not bytes), appending `…` when shortened.
/// Char-based so it never splits a multi-byte UTF-8 sequence.
pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    }
}

/// Return the last top-level brace-balanced `{...}` substring of `s`, if any. Used to pull a
/// verdict / summary JSON object out of model output that may have prose or a ```json fence
/// around it. Tolerant: scans from the last `}` back to its matching `{`.
pub fn last_json_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let end = (0..bytes.len()).rev().find(|&i| bytes[i] == b'}')?;
    let mut depth = 0i32;
    for i in (0..=end).rev() {
        match bytes[i] {
            b'}' => depth += 1,
            b'{' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[i..=end]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_epoch_is_plausible() {
        // after 2020-01-01 and before some far-future date — sanity, not precision.
        let t = now_epoch();
        assert!(t > 1_577_836_800, "epoch {t} should be after 2020");
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_shortens_with_ellipsis() {
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn truncate_is_char_safe_on_multibyte() {
        // 'é' is 2 bytes; truncating must not split it.
        let s = "ééééé"; // 5 chars, 10 bytes
        assert_eq!(truncate(s, 3), "ééé…");
    }

    #[test]
    fn last_json_object_picks_trailing_block() {
        let s = "prose {not: valid} more\nthen {\"met\":false,\"value\":1}";
        assert_eq!(last_json_object(s), Some(r#"{"met":false,"value":1}"#));
    }

    #[test]
    fn last_json_object_handles_nesting() {
        let s = "noise {\"a\":{\"b\":1}} tail";
        assert_eq!(last_json_object(s), Some(r#"{"a":{"b":1}}"#));
    }

    #[test]
    fn last_json_object_none_when_absent() {
        assert_eq!(last_json_object("no braces here"), None);
    }

    #[test]
    fn fmt_dur_covers_all_three_magnitudes() {
        assert_eq!(fmt_dur(11_520), "3h12m"); // the shape all three copies agreed on
        assert_eq!(fmt_dur(303), "5m03s"); // status used to print a lossy "5m" here
        assert_eq!(fmt_dur(45), "45s"); // ...and a bare "0m" here, which read as no uptime
        assert_eq!(fmt_dur(0), "0s");
    }

    #[test]
    fn human_bytes_switches_unit_at_1k() {
        assert_eq!(human_bytes(640), "640 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
    }
}
