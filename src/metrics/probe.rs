//! The startup checks of an OPTIONAL dependency: a metric or score judge refuses the run when
//! `swe-metrics` is not on `PATH`, speaks another schema version, or — for a score judge — scores
//! by another definition or version than the one configured. A config without such judges never
//! runs any of this, which is what keeps agg working without swe-metrics installed.

use super::measure::PROGRAM;
use super::report::REPORT_SCHEMA;
use super::score::{ScoreDoc, SCORE_SCHEMA};
use super::spec::ScoreJudge;
use crate::problem::Problem;
use std::process::{Command, Stdio};

/// How long one probe may take (python's start-up, not a measurement).
const PROBE_SECS: u64 = 60;

/// Run `swe-metrics <args>` for a probe: its stdout, or why it could not.
fn call(args: &[&str]) -> Result<String, String> {
    let mut c = Command::new(PROGRAM);
    c.args(args).stdin(Stdio::null());
    let out = crate::os::proc::run_with_timeout(c, PROBE_SECS).map_err(|e| {
        if e.contains("No such file") || e.contains("not found") {
            format!("`{PROGRAM}` is not on PATH — metric and score judges need swe-metrics installed (it is optional otherwise)")
        } else {
            format!("`{PROGRAM} {}` could not run: {e}", args.join(" "))
        }
    })?;
    if !out.success {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("`{PROGRAM} {}` failed: {}", args.join(" "), err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The `$id` of `swe-metrics schema <which>` must be the one this agg reads.
fn schema(which: &str, want: &str) -> Result<(), String> {
    let text = call(&["schema", which])?;
    let id = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("$id").and_then(|i| i.as_str()).map(String::from))
        .unwrap_or_default();
    if id == want {
        Ok(())
    } else {
        Err(format!("swe-metrics speaks the {which} schema `{id}`, but this agg reads `{want}` — install a compatible swe-metrics"))
    }
}

/// A score of an EMPTY report: what definition and version the configured one really is.
fn definition_of(j: &ScoreJudge) -> Result<ScoreDoc, String> {
    let dir = std::env::temp_dir().join(format!("agg-swe-metrics-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("probe dir: {e}"))?;
    let report = dir.join("empty-report.json");
    let empty = serde_json::json!({"schema": REPORT_SCHEMA, "subject": {"target": ".", "kind": "directory", "commit": null,
        "changed_since": null, "files": 0, "languages": {}}, "analyzers": [], "metrics": [], "details": []});
    std::fs::write(&report, empty.to_string()).map_err(|e| format!("probe report: {e}"))?;
    let mut args = vec!["score".to_string(), report.display().to_string()];
    args.extend(j.definition.args());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = call(&refs);
    let _ = std::fs::remove_dir_all(&dir);
    ScoreDoc::parse(&out?)
}

/// Every startup problem of the metric/score judges named here (`(name, is_score, score spec)`),
/// located at `judges.<name>`. Empty when there are none — swe-metrics is then never called.
pub fn problems(metric: &[&str], score: &[(&str, &ScoreJudge)]) -> Vec<Problem> {
    let Some(first) = metric.first().or(score.first().map(|(n, _)| n)) else { return vec![] };
    let names: Vec<&str> = metric.iter().copied().chain(score.iter().map(|(n, _)| *n)).collect();
    let at = format!("judges.{first}");
    if let Err(e) = schema("report", REPORT_SCHEMA) {
        return vec![Problem::new(at, format!("{e} (judges: {})", names.join(", ")))];
    }
    if score.is_empty() {
        return vec![];
    }
    if let Err(e) = schema("score", SCORE_SCHEMA) {
        return vec![Problem::new(at, e)];
    }
    score
        .iter()
        .filter_map(|(name, j)| {
            let checked = definition_of(j).and_then(|d| d.check_definition(j));
            checked.err().map(|e| Problem::new(format!("judges.{name}.score"), e))
        })
        .collect()
}
