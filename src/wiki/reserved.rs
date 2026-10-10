//! The two reserved filenames (OKF v0.2 §3.1): `index.md`, a directory listing (§8), and `log.md`,
//! a newest-first history (§9). Neither is a concept, so neither needs a `type`; each has a
//! structure of its own instead, and §11 makes following it part of conformance.

use super::concept::{self, Split};
use super::Findings;

/// The one key a bundle-ROOT `index.md` may carry in frontmatter (§8, §12).
const OKF_VERSION_KEY: &str = "okf_version";

/// Lint an `index.md`. `is_root` = it sits at the bundle root, the only place it may have
/// frontmatter, and then only `okf_version`.
pub(super) fn check_index(file: &str, text: &str, is_root: bool, out: &mut Findings) {
    let Some((body, first_line)) = index_body(file, text, is_root, out) else { return };
    let mut seen_heading = false;
    for (i, line) in body.lines().enumerate() {
        let n = first_line + i;
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if heading_level(t).is_some() {
            seen_heading = true;
        } else if let Some(item) = list_item(t) {
            if !seen_heading {
                out.problem(file, Some(n), "index.entry", "an index entry sits under a section heading (`# Group`) (OKF §8)");
            }
            if !starts_with_link(item) {
                out.problem(file, Some(n), "index.entry", "an index entry is a link: `* [Title](page.md) - description` (OKF §8)");
            }
        } else {
            out.warning_at(file, n, "index.text", "free text in an index: §8 lists entries under headings");
        }
    }
    if !seen_heading {
        out.problem(file, None, "index.sections", "an index has one or more sections, each under a `#` heading (OKF §8)");
    }
}

/// The body of an `index.md` and the line it starts on, after vetting its frontmatter. `None` when
/// the frontmatter is broken beyond reading the rest.
fn index_body<'a>(file: &str, text: &'a str, is_root: bool, out: &mut Findings) -> Option<(&'a str, usize)> {
    match concept::split(text) {
        Split::None => Some((text, 1)),
        Split::Unterminated => {
            out.problem(file, Some(1), "frontmatter.unterminated", "the frontmatter has no closing `---` line");
            None
        }
        Split::Block { yaml, body, body_line } => {
            if !is_root {
                out.problem(file, Some(1), "index.frontmatter", "an index below the bundle root has no frontmatter (OKF §8)");
                return Some((body, body_line));
            }
            match concept::parse(yaml) {
                Err((line, msg)) => out.problem(file, line, "frontmatter.yaml", &msg),
                Ok(map) => {
                    let extra = map.iter().flatten().any(|(k, _)| k.as_str() != Some(OKF_VERSION_KEY));
                    if extra {
                        out.problem(file, Some(1), "index.frontmatter", "the root index's frontmatter holds only `okf_version` (OKF §8, §12)");
                    }
                }
            }
            Some((body, body_line))
        }
    }
}

/// Lint a `log.md`: `## YYYY-MM-DD` groups, newest first, every entry inside a group (§9). A
/// leading `#` title is allowed, as in the spec's own example.
pub(super) fn check_log(file: &str, text: &str, out: &mut Findings) {
    let (body, first_line) = match concept::split(text) {
        Split::Block { body, body_line, .. } => (body, body_line),
        _ => (text, 1),
    };
    let mut last: Option<jiff::civil::Date> = None;
    let mut groups = 0;
    for (i, line) in body.lines().enumerate() {
        let n = first_line + i;
        let t = line.trim();
        match heading_level(t) {
            _ if t.is_empty() => {}
            Some(1) => {}
            Some(level) => {
                groups += 1;
                last = check_date(file, n, t[level..].trim(), last, out).or(last);
            }
            None if groups == 0 => {
                out.problem(file, Some(n), "log.entry", "a log entry sits under a `## YYYY-MM-DD` heading (OKF §9)");
            }
            None => {}
        }
    }
    if groups == 0 {
        out.warning(file, "log.empty", "a log holds date-grouped entries under `## YYYY-MM-DD` headings (OKF §9)");
    }
}

/// Vet one date heading against the previous one; returns the date when it parses.
fn check_date(file: &str, n: usize, text: &str, last: Option<jiff::civil::Date>, out: &mut Findings) -> Option<jiff::civil::Date> {
    let Some(date) = iso_date(text) else {
        out.problem(file, Some(n), "log.date", &format!("`{text}` is not a date heading: they are ISO 8601 `YYYY-MM-DD` (OKF §9)"));
        return None;
    };
    if let Some(prev) = last {
        if date >= prev {
            out.problem(file, Some(n), "log.order", &format!("{date} follows {prev}: a log is newest first, one group per date (OKF §9)"));
        }
    }
    Some(date)
}

/// `YYYY-MM-DD` exactly — a real calendar date, zero-padded, nothing after it.
pub(super) fn iso_date(s: &str) -> Option<jiff::civil::Date> {
    let b = s.as_bytes();
    let shape = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if !shape {
        return None;
    }
    s.parse::<jiff::civil::Date>().ok()
}

/// The ATX heading level of a trimmed line (`## x` → 2), `None` for a non-heading.
fn heading_level(t: &str) -> Option<usize> {
    let level = t.bytes().take_while(|&c| c == b'#').count();
    let rest = &t[level..];
    (level > 0 && level <= 6 && (rest.is_empty() || rest.starts_with(' '))).then_some(level)
}

/// The content of a markdown bullet (`* x`, `- x`, `+ x`), `None` for anything else.
fn list_item(t: &str) -> Option<&str> {
    ["* ", "- ", "+ "].iter().find_map(|m| t.strip_prefix(m)).map(str::trim_start)
}

/// Does the entry open with a markdown link `[label](target)`?
fn starts_with_link(item: &str) -> bool {
    let Some(rest) = item.strip_prefix('[') else { return false };
    rest.find("](").is_some_and(|i| rest[i + 2..].contains(')'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_read_markdown_shapes() {
        assert_eq!(heading_level("## 2026-01-02"), Some(2));
        assert_eq!(heading_level("#"), Some(1));
        assert_eq!(heading_level("#hashtag"), None, "no space: not a heading");
        assert_eq!(heading_level("####### seven"), None);
        assert_eq!(list_item("* [a](b.md)"), Some("[a](b.md)"));
        assert_eq!(list_item("plain"), None);
        assert!(starts_with_link("[Plan](plan.md) - the plan"));
        assert!(!starts_with_link("Plan (plan.md)"));
        assert!(!starts_with_link("[Plan] plan.md"));
        assert!(iso_date("2026-02-28").is_some());
        assert!(iso_date("2026-02-30").is_none(), "not a calendar date");
        assert!(iso_date("2026-2-28").is_none(), "not zero-padded");
        assert!(iso_date("2026-02-28 notes").is_none());
    }
}
