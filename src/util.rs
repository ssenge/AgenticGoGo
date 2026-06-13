//! Small pure helpers shared across modules. Previously copy-pasted: `now_epoch` lived in
//! four files, `truncate` in three (one copy had already silently diverged), and
//! `last_json_object` was byte-identical in judge.rs and summary.rs. One home, one definition.

/// Seconds since the Unix epoch (UTC). 0 if the clock is somehow before the epoch.
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
}
