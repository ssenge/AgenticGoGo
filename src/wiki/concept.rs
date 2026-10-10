//! One OKF concept document: its frontmatter block (OKF v0.2 §4) and the optional families whose
//! shape §5 fixes.
//!
//! Conformance (§11) asks three things of a concept: a frontmatter block, that it parses as YAML, and
//! a non-empty `type`. Those are PROBLEMS. Everything §5 says about `status`, `generated`, `verified`,
//! `stale_after` and `sources` is producer guidance (§11: "producers SHOULD follow §5 through §10"),
//! so a malformed family is a WARNING — a consumer must still accept the concept.

use super::Findings;
use serde_yaml::{Mapping, Value};

/// A markdown file cut at its frontmatter fences.
#[derive(Debug, PartialEq)]
pub(super) enum Split<'a> {
    /// the first line is not `---`: there is no frontmatter block.
    None,
    /// an opening `---` with no closing one.
    Unterminated,
    /// `yaml` sits between the fences; `body` follows the closing fence, starting on `body_line`.
    Block { yaml: &'a str, body: &'a str, body_line: usize },
}

/// Cut `text` at its frontmatter fences. A leading byte-order mark is tolerated.
pub(super) fn split(text: &str) -> Split<'_> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let start = match lines.next() {
        Some(first) if is_fence(first) => first.len(),
        _ => return Split::None,
    };
    let mut offset = start;
    for (i, line) in lines.enumerate() {
        if is_fence(line) {
            let body = &text[offset + line.len()..];
            // line 1 is the opening fence, so the closing fence is line i + 2.
            return Split::Block { yaml: &text[start..offset], body, body_line: i + 3 };
        }
        offset += line.len();
    }
    Split::Unterminated
}

fn is_fence(line: &str) -> bool {
    line.trim_end() == "---"
}

/// Parse a frontmatter block. `Ok(None)` is an EMPTY block (`---` straight after `---`), which is a
/// mapping with no keys; `Err` carries the 1-based line of the YAML error, when known.
pub(super) fn parse(yaml: &str) -> Result<Option<Mapping>, (Option<usize>, String)> {
    match serde_yaml::from_str::<Value>(yaml) {
        Ok(Value::Null) => Ok(None),
        Ok(Value::Mapping(m)) => Ok(Some(m)),
        Ok(_) => Err((None, "the frontmatter is YAML but not a mapping of keys".into())),
        // +1: the YAML starts on line 2, after the opening fence.
        Err(e) => Err((e.location().map(|l| l.line() + 1), format!("the frontmatter is not valid YAML: {e}"))),
    }
}

/// Lint one concept document (any `.md` that is not a reserved filename).
pub(super) fn check(file: &str, text: &str, worker_wiki: bool, out: &mut Findings) {
    let yaml = match split(text) {
        Split::None => {
            let msg = "no YAML frontmatter: a concept opens with a `---` block carrying a non-empty `type` (OKF §4.1)";
            return out.problem(file, Some(1), "frontmatter.missing", msg);
        }
        Split::Unterminated => {
            return out.problem(file, Some(1), "frontmatter.unterminated", "the frontmatter has no closing `---` line");
        }
        Split::Block { yaml, .. } => yaml,
    };
    let map = match parse(yaml) {
        Ok(m) => m.unwrap_or_default(),
        Err((line, msg)) => return out.problem(file, line, "frontmatter.yaml", &msg),
    };
    check_type(file, &map, out);
    families(file, &map, worker_wiki, out);
}

/// `type` is the one always-required key (§4.1): present, a string, not blank.
fn check_type(file: &str, map: &Mapping, out: &mut Findings) {
    match map.get("type") {
        None => out.problem(file, None, "type.missing", "the frontmatter has no `type` — it is required (OKF §4.1)"),
        Some(Value::String(s)) if !s.trim().is_empty() => {}
        Some(Value::String(_)) | Some(Value::Null) => out.problem(file, None, "type.empty", "`type` is empty"),
        Some(_) => out.problem(file, None, "type.invalid", "`type` must be a string (a short name such as `decision`)"),
    }
}

/// The optional §5 families. Producer guidance, so every finding here is a warning.
fn families(file: &str, map: &Mapping, worker_wiki: bool, out: &mut Findings) {
    if let Some(v) = map.get("status") {
        if !matches!(v.as_str(), Some("draft" | "stable" | "deprecated")) {
            out.warning(file, "status.unknown", "`status` is one of draft | stable | deprecated (OKF §5.4)");
        }
    }
    if let Some(v) = map.get("generated") {
        check_generated(file, v, out);
    }
    if let Some(v) = map.get("verified") {
        check_verified(file, v, worker_wiki, out);
    }
    if let Some(v) = map.get("stale_after") {
        check_instant(file, "stale_after", v, out);
    }
    if let Some(v) = map.get("sources") {
        check_sources(file, v, out);
    }
    if map.contains_key("timestamp") {
        out.warning(file, "timestamp.legacy", "`timestamp` is OKF v0.1; v0.2 records it as `generated: { by, at }` (§13.1)");
    }
}

/// `generated: { by: <actor>, at: <instant> }` — `by` is required within it (§5.2).
fn check_generated(file: &str, v: &Value, out: &mut Findings) {
    let Some(m) = v.as_mapping() else {
        return out.warning(file, "generated.shape", "`generated` is a mapping `{ by, at }` (OKF §5.2)");
    };
    if !names_actor(m) {
        out.warning(file, "generated.by", "`generated.by` is required within `generated` — an actor such as `agent/model` (OKF §5.2, §7)");
    }
    if let Some(at) = m.get("at") {
        check_instant(file, "generated.at", at, out);
    }
}

/// `verified` is one `{ by, at }` mapping or a list of them (§5.2). In the project's own wiki it is
/// also a claim nobody made: the worker writes that wiki, and verification is a human act outside agg.
fn check_verified(file: &str, v: &Value, worker_wiki: bool, out: &mut Findings) {
    if worker_wiki {
        out.warning(file, "verified.worker", "`verified` in a worker-written wiki: verification is a reviewer's act, and neither agg nor the worker makes it");
    }
    let entries: Vec<&Value> = match v {
        Value::Sequence(s) => s.iter().collect(),
        other => vec![other],
    };
    for e in entries {
        let Some(m) = e.as_mapping() else {
            out.warning(file, "verified.shape", "each `verified` entry is a mapping `{ by, at }` (OKF §5.2)");
            continue;
        };
        if !names_actor(m) {
            out.warning(file, "verified.shape", "each `verified` entry names its actor in `by` (OKF §5.2)");
        }
        match m.get("at") {
            Some(at) => check_instant(file, "verified.at", at, out),
            None => out.warning(file, "verified.shape", "each `verified` entry carries `at` (OKF §5.2)"),
        }
    }
}

/// `sources` is a list of entries, each with a `resource` (§5.1).
fn check_sources(file: &str, v: &Value, out: &mut Findings) {
    let Some(list) = v.as_sequence() else {
        return out.warning(file, "sources.shape", "`sources` is a list of entries (OKF §5.1)");
    };
    let missing = list.iter().any(|e| e.as_mapping().is_none_or(|m| m.get("resource").is_none_or(Value::is_null)));
    if missing {
        out.warning(file, "sources.resource", "every `sources` entry needs a `resource` (OKF §5.1)");
    }
}

/// Does a `{ by, … }` mapping name its actor (a non-blank `by`, §7)?
fn names_actor(m: &Mapping) -> bool {
    m.get("by").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty())
}

/// An ISO 8601 datetime with an explicit UTC offset, e.g. `2026-06-30T14:00:00Z` (§5).
fn check_instant(file: &str, key: &str, v: &Value, out: &mut Findings) {
    if !v.as_str().is_some_and(is_instant) {
        out.warning(file, "timestamp.format", &format!("`{key}` is an ISO 8601 datetime with an explicit offset, e.g. 2026-06-30T14:00:00Z (OKF §5)"));
    }
}

/// True for an RFC 3339 / ISO 8601 instant that names its offset (`Z` or `±hh:mm`).
pub(super) fn is_instant(s: &str) -> bool {
    s.trim().parse::<jiff::Timestamp>().is_ok()
}
