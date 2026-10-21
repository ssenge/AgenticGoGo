//! The swe-metrics score document (`swe-metrics/score/v1`) and what a score judge concludes from
//! it — pure, except [`run`], which invokes `swe-metrics score`.

use super::combine::Threshold;
use super::measure::{self, Ctx};
use super::spec::ScoreJudge;
use serde::Deserialize;
use std::path::Path;

/// The score schema this agg reads.
pub const SCORE_SCHEMA: &str = "swe-metrics/score/v1";

/// The parts of a score document agg reads.
#[derive(Debug, Clone, Deserialize)]
pub struct ScoreDoc {
    pub schema: String,
    pub definition: DefinitionId,
    /// `null` = incomplete; `notes` say why.
    pub score: Option<f64>,
    #[serde(default)]
    pub gates: Vec<GateRow>,
    #[serde(default)]
    pub components: Vec<ComponentRow>,
    #[serde(default)]
    pub regressions: Vec<RegressionRow>,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// The definition a score was computed by.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DefinitionId {
    pub name: String,
    pub version: String,
}

/// One gate of the definition (a hard limit that caps the score while it fails).
#[derive(Debug, Clone, Deserialize)]
pub struct GateRow {
    pub metric: String,
    pub passed: bool,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub cap: Option<f64>,
    #[serde(default)]
    pub status: Option<String>,
}

/// One weighted quality part of the score.
#[derive(Debug, Clone, Deserialize)]
pub struct ComponentRow {
    pub metric: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub good: Option<f64>,
    #[serde(default)]
    pub bad: Option<f64>,
    #[serde(default)]
    pub weight: f64,
    /// 0–100 between `bad` and `good`; none when the part wasn't measured.
    #[serde(default)]
    pub subscore: Option<f64>,
    #[serde(default)]
    pub status: Option<String>,
}

/// One metric compared against the baseline report.
#[derive(Debug, Clone, Deserialize)]
pub struct RegressionRow {
    pub metric: String,
    pub worse: bool,
}

impl ScoreDoc {
    /// Parse a score document, refusing one of another schema version.
    pub fn parse(text: &str) -> Result<ScoreDoc, String> {
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not a swe-metrics score: {e}"))?;
        let schema = v.get("schema").and_then(|s| s.as_str()).unwrap_or_default();
        if schema != SCORE_SCHEMA {
            return Err(format!("score schema `{schema}`, but this agg reads `{SCORE_SCHEMA}`"));
        }
        serde_json::from_value(v).map_err(|e| format!("not a swe-metrics score: {e}"))
    }

    /// Refuse a score of another definition or version than `j` configures: two definitions'
    /// scores are not comparable, and a silently swapped definition would move the ratchet.
    pub fn check_definition(&self, j: &ScoreJudge) -> Result<(), String> {
        let named = matches!(j.definition, super::spec::Definition::Default);
        let name_ok = !named || self.definition.name == super::spec::DEFAULT_DEFINITION;
        if name_ok && self.definition.version == j.version {
            return Ok(());
        }
        Err(format!(
            "the score was computed by `{}` version \"{}\", but the judge is configured for `{}` version \"{}\" — \
             scores of different definitions are not comparable, so this one is refused",
            self.definition.name, self.definition.version, j.definition_name, j.version
        ))
    }
}

/// No part of the score applies to the measured files (e.g. only a shell script changed): every
/// component is `not_applicable` and no gate fails. A part that failed to measure is not this.
pub fn nothing_to_score(doc: &ScoreDoc) -> bool {
    let na = |s: &Option<String>| s.as_deref() == Some("not_applicable");
    doc.score.is_none()
        && !doc.components.is_empty()
        && doc.components.iter().all(|c| na(&c.status))
        && doc.gates.iter().all(|g| g.passed && (na(&g.status) || g.status.as_deref() == Some("ok")))
}

/// The verdict parts a score judge concludes: `(value, met, rationale)` — or the error.
pub fn conclude(doc: &ScoreDoc, j: &ScoreJudge) -> Result<(f64, bool, String), String> {
    doc.check_definition(j)?;
    let Some(score) = doc.score else {
        let notes = if doc.notes.is_empty() { "no notes".to_string() } else { doc.notes.join("; ") };
        return Err(format!("the score is incomplete (null): {notes}"));
    };
    let t = Threshold { max: j.max, min: j.min };
    let met = t.passes(score);
    let mut why = format!(
        "score {score} ({}@{}) {} {}",
        doc.definition.name,
        doc.definition.version,
        if met { "meets" } else { "misses" },
        t.describe()
    );
    let failed: Vec<String> = doc.gates.iter().filter(|g| !g.passed).map(describe_gate).collect();
    if !failed.is_empty() {
        why.push_str(&format!("; capped by failed gates: {}", failed.join(", ")));
    }
    let lost = costliest(&doc.components);
    if !met && !lost.is_empty() {
        why.push_str(&format!("; most points lost: {}", lost.join(", ")));
    }
    let worse: Vec<&str> = doc.regressions.iter().filter(|r| r.worse).map(|r| r.metric.as_str()).collect();
    if !worse.is_empty() {
        why.push_str(&format!("; worse than the incumbent: {}", worse.join(", ")));
    }
    Ok((score, met, why))
}

/// The three parts that cost the most points (weight × what they miss of 100), worst first:
/// what a worker repairs first when the score misses its threshold.
fn costliest(components: &[ComponentRow]) -> Vec<String> {
    let lost = |c: &ComponentRow| c.subscore.map_or(0.0, |s| c.weight * (100.0 - s));
    let mut short: Vec<&ComponentRow> = components.iter().filter(|c| lost(c) > 0.0).collect();
    short.sort_by(|a, b| lost(b).total_cmp(&lost(a)));
    short.iter().take(3).map(|c| {
        let value = c.value.map_or("unavailable".into(), |v| format!("= {v}"));
        match (c.good, c.bad) {
            (Some(g), Some(b)) => format!("{} {value} (good {g}, bad {b})", c.metric),
            _ => format!("{} {value}", c.metric),
        }
    }).collect()
}

/// `secrets.findings = 2 (max 0, caps at 40)`.
fn describe_gate(g: &GateRow) -> String {
    let value = match (g.value, g.status.as_deref()) {
        (Some(v), _) => format!("= {v}"),
        (None, Some(s)) => s.to_string(),
        (None, None) => "unavailable".into(),
    };
    let mut limit = Vec::new();
    if let Some(m) = g.max {
        limit.push(format!("max {m}"));
    }
    if let Some(m) = g.min {
        limit.push(format!("min {m}"));
    }
    if let Some(c) = g.cap {
        limit.push(format!("caps at {c}"));
    }
    format!("{} {value} ({})", g.metric, limit.join(", "))
}

/// Score `report` by `j`'s definition, against `baseline` when there is one; the document is
/// written to `out` (kept as evidence) and returned.
pub fn run(c: &Ctx<'_>, j: &ScoreJudge, report: &Path, baseline: Option<&Path>, out: &Path) -> Result<ScoreDoc, String> {
    let mut args: Vec<String> = vec!["score".into(), report.display().to_string()];
    if let Some(b) = baseline {
        args.extend(["--baseline".into(), b.display().to_string()]);
    }
    args.extend(j.definition.args());
    args.extend(["--out".into(), out.display().to_string()]);
    // a definition FILE outside the project is mounted for a container (inside, it is the project's).
    let outside: Vec<std::path::PathBuf> = match &j.definition {
        super::spec::Definition::File(p) => super::contained::outside_project(c.cwd, p).into_iter().collect(),
        _ => vec![],
    };
    measure::run_tool(c, &args, &outside).map_err(|e| format!("`swe-metrics score` failed: {e}"))?;
    let text = std::fs::read_to_string(out).map_err(|e| format!("swe-metrics wrote no score: {e}"))?;
    ScoreDoc::parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(score: serde_json::Value, version: &str, gates: serde_json::Value, extra: serde_json::Value) -> ScoreDoc {
        let mut d = serde_json::json!({"schema": SCORE_SCHEMA, "definition": {"name": "swe-default", "version": version},
            "score": score, "gates": gates, "components": [], "regressions": [], "notes": []});
        for (k, v) in extra.as_object().unwrap() {
            d[k] = v.clone();
        }
        ScoreDoc::parse(&d.to_string()).unwrap()
    }

    fn judge() -> ScoreJudge {
        ScoreJudge::new("swe-default", "2").min(80.0)
    }

    #[test]
    fn a_complete_score_is_the_value_and_meets_its_threshold() {
        let (v, met, why) = conclude(&doc(serde_json::json!(85.5), "2", serde_json::json!([]), serde_json::json!({})), &judge()).unwrap();
        assert_eq!((v, met), (85.5, true));
        assert!(why.contains("score 85.5 (swe-default@2) meets ≥ 80"), "{why}");
    }

    /// A gate-capped score names the failed gates; a regression against the incumbent is named too.
    #[test]
    fn a_capped_score_names_its_failed_gates() {
        let gates = serde_json::json!([
            {"metric": "secrets.findings", "passed": false, "value": 2.0, "max": 0, "cap": 40.0, "status": "ok"},
            {"metric": "tests.failed", "passed": true, "value": 0.0, "max": 0, "cap": 40.0},
            {"metric": "iac.validation_errors", "passed": false, "value": null, "max": 0, "cap": 40.0, "status": "unavailable"},
        ]);
        let regs = serde_json::json!({"regressions": [{"metric": "duplication.percent", "worse": true}, {"metric": "x.y", "worse": false}]});
        let (v, met, why) = conclude(&doc(serde_json::json!(40.0), "2", gates, regs), &judge()).unwrap();
        assert_eq!((v, met), (40.0, false));
        assert!(why.contains("capped by failed gates: secrets.findings = 2 (max 0, caps at 40), iac.validation_errors unavailable"), "{why}");
        assert!(why.contains("worse than the incumbent: duplication.percent") && !why.contains("x.y"), "{why}");
    }

    /// A missed score names the parts that cost the most points, worst first; a met one doesn't.
    #[test]
    fn a_missed_score_names_its_costliest_parts() {
        let parts = serde_json::json!({"components": [
            {"metric": "doc_coverage.percent", "value": 60.0, "good": 80, "bad": 30, "weight": 1, "subscore": 60.0},
            {"metric": "complexity.cognitive [p95]", "value": 12.0, "good": 5, "bad": 15, "weight": 3, "subscore": 30.0},
            {"metric": "duplication.percent", "value": 0.0, "good": 3, "bad": 10, "weight": 2, "subscore": 100.0},
            {"metric": "coverage.line_percent", "weight": 3}]});
        let (_, met, why) = conclude(&doc(serde_json::json!(70.0), "2", serde_json::json!([]), parts.clone()), &judge()).unwrap();
        assert!(!met);
        assert!(why.ends_with("most points lost: complexity.cognitive [p95] = 12 (good 5, bad 15), \
                               doc_coverage.percent = 60 (good 80, bad 30)"), "{why}");
        let (_, met, why) = conclude(&doc(serde_json::json!(90.0), "2", serde_json::json!([]), parts), &judge()).unwrap();
        assert!(met && !why.contains("points lost"), "{why}");
    }

    /// A `null` score is an ERROR carrying the score's notes — never met.
    #[test]
    fn a_null_score_is_an_error_with_its_notes() {
        let d = doc(serde_json::json!(null), "2", serde_json::json!([]), serde_json::json!({"notes": ["component coverage: cobertura failed", "score incomplete"]}));
        let e = conclude(&d, &judge()).unwrap_err();
        assert!(e.contains("incomplete") && e.contains("cobertura failed; score incomplete"), "{e}");
    }

    /// Another version, or another definition than the shipped one it was told, is refused.
    #[test]
    fn a_definition_or_version_mismatch_is_refused() {
        let e = conclude(&doc(serde_json::json!(90.0), "1", serde_json::json!([]), serde_json::json!({})), &judge()).unwrap_err();
        assert!(e.contains("version \"1\"") && e.contains("not comparable"), "{e}");
        let mut other = doc(serde_json::json!(90.0), "2", serde_json::json!([]), serde_json::json!({}));
        other.definition.name = "team-a".into();
        assert!(conclude(&other, &judge()).is_err(), "swe-default was configured");
        // a file/module definition is checked on its version (its name is the file's own).
        let file = ScoreJudge { definition: super::super::spec::Definition::Module("t.s:D".into()), ..judge() };
        assert!(conclude(&other, &file).is_ok());
        assert!(ScoreDoc::parse(r#"{"schema":"swe-metrics/score/v9"}"#).unwrap_err().contains("v9"));
    }

    #[test]
    fn nothing_to_score_only_when_every_part_is_not_applicable() {
        let na = serde_json::json!({"metric": "complexity.cyclomatic p95", "status": "not_applicable", "weight": 3});
        let gate = |passed: bool, status: &str| serde_json::json!([{"metric": "secrets.findings", "passed": passed, "status": status}]);
        let with = |score, gates, comps| doc(score, "2", gates, serde_json::json!({"components": comps}));
        let null = serde_json::Value::Null;
        assert!(nothing_to_score(&with(null.clone(), gate(true, "ok"), serde_json::json!([na]))));
        assert!(!nothing_to_score(&with(null.clone(), gate(false, "ok"), serde_json::json!([na]))), "a failing gate");
        assert!(!nothing_to_score(&with(null.clone(), gate(true, "unavailable"), serde_json::json!([na]))), "a gate not measured");
        let failed = serde_json::json!({"metric": "coverage.line_percent", "status": "unavailable", "weight": 3});
        assert!(!nothing_to_score(&with(null.clone(), gate(true, "ok"), serde_json::json!([na, failed]))), "a part that failed");
        assert!(!nothing_to_score(&with(null, gate(true, "ok"), serde_json::json!([]))), "no parts at all");
        assert!(!nothing_to_score(&with(serde_json::json!(80.0), gate(true, "ok"), serde_json::json!([na]))), "a score");
    }

}
