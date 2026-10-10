//! The swe-metrics report (`swe-metrics/report/v1`) as agg reads it, and the SELECTION of one
//! metric's value per tool out of it — pure, so every rule is a unit test.
//!
//! The selection rules mirror swe-metrics' own scoring lookup (`swe_metrics/scoring/values.py`), so
//! a metric judge and a score read the same report the same way:
//! - only TARGET-level rows count (module/file/function rows are details);
//! - without `labels`, the unlabeled total; with them, every row whose labels match (any accepted
//!   value), SUMMED — `severity: [critical, high]` is the two rows added;
//! - a labeled count with no matching row but a present total is a measured `0` (label rows exist
//!   only for labels that occurred);
//! - a statistic reads `stats.<stat>`; a single-value metric has no statistics.
//!
//! What is agg's own is FAIL-CLOSED: a value that is missing is an error that carries the reason
//! the report gives (the analyzer's `skipped`/`failed` reason), never a pass.

use super::spec::{family, Stat};
use serde::Deserialize;
use std::collections::BTreeMap;

/// The report schema this agg reads.
pub const REPORT_SCHEMA: &str = "swe-metrics/report/v1";

/// One report, with the parts agg reads. Unknown fields are ignored: the schema is versioned by its
/// `schema` id, which [`Report::parse`] checks.
#[derive(Debug, Clone, Deserialize)]
pub struct Report {
    pub schema: String,
    #[serde(default)]
    pub subject: Option<Subject>,
    #[serde(default)]
    pub analyzers: Vec<Analyzer>,
    #[serde(default)]
    pub metrics: Vec<Row>,
    #[serde(default)]
    pub details: Vec<Detail>,
}

/// What was measured.
#[derive(Debug, Clone, Deserialize)]
pub struct Subject {
    /// set when only the files changed since this Git ref were measured (`--changed-since`).
    #[serde(default)]
    pub changed_since: Option<String>,
    /// how many files were measured.
    #[serde(default)]
    pub files: Option<u64>,
}

/// The reason swe-metrics' runner gives an analyzer that found no file it applies to
/// (`runner/measure.py`). There is no reason CODE in the report, so this exact constant is the
/// signal; if swe-metrics ever words it differently, [`nothing_to_measure`] stops matching and the
/// judge errors again — it fails closed, never open.
pub const NO_APPLICABLE_FILES: &str = "no applicable files";

/// One analyzer's run.
#[derive(Debug, Clone, Deserialize)]
pub struct Analyzer {
    pub id: String,
    pub tool: String,
    /// `ran` | `skipped` | `failed`.
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub families: Vec<String>,
}

/// One metric row.
#[derive(Debug, Clone, Deserialize)]
pub struct Row {
    pub id: String,
    pub tool: String,
    /// `lower_better` | `higher_better` | `neutral`.
    pub direction: String,
    pub scope: RowScope,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub stats: Option<BTreeMap<String, f64>>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// Where a row applies.
#[derive(Debug, Clone, Deserialize)]
pub struct RowScope {
    pub level: String,
}

/// One kept detailed output (a SARIF file), relative to the `--details` directory.
#[derive(Debug, Clone, Deserialize)]
pub struct Detail {
    pub analyzer: String,
    #[serde(default)]
    pub format: String,
    pub path: String,
}

/// Which way is better for a metric, as the report states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Better {
    Lower,
    Higher,
    Neutral,
}

impl Better {
    fn parse(s: &str) -> Better {
        match s {
            "lower_better" => Better::Lower,
            "higher_better" => Better::Higher,
            _ => Better::Neutral,
        }
    }

    /// The report's spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Better::Lower => "lower_better",
            Better::Higher => "higher_better",
            Better::Neutral => "neutral",
        }
    }
}

impl Report {
    /// Parse a report, refusing one of another schema version.
    pub fn parse(text: &str) -> Result<Report, String> {
        let r: Report = serde_json::from_str(text).map_err(|e| format!("not a swe-metrics report: {e}"))?;
        if r.schema != REPORT_SCHEMA {
            return Err(format!("report schema `{}`, but this agg reads `{REPORT_SCHEMA}`", r.schema));
        }
        Ok(r)
    }

    /// The analyzers that measure `family` (and, with `tool`, are that tool).
    fn analyzers_of<'a>(&'a self, family: &'a str, tool: Option<&'a str>) -> impl Iterator<Item = &'a Analyzer> + 'a {
        self.analyzers
            .iter()
            .filter(move |a| a.families.iter().any(|f| f == family) && tool.is_none_or(|t| a.tool == t || a.id == t))
    }

    /// The details (SARIF and the like) of the analyzers that measure `family`; all of them for `None`.
    pub fn details_of(&self, family: Option<&str>) -> Vec<&Detail> {
        self.details
            .iter()
            .filter(|d| match family {
                None => true,
                Some(f) => self.analyzers.iter().any(|a| a.id == d.analyzer && a.families.iter().any(|x| x == f)),
            })
            .collect()
    }
}

/// What a metric judge selects.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub id: String,
    pub tool: Option<String>,
    pub stat: Option<Stat>,
    /// key → accepted values.
    pub labels: Vec<(String, Vec<String>)>,
}

/// One tool's value for the selected metric.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolValue {
    pub tool: String,
    pub value: f64,
    pub better: Better,
}

impl Selector {
    /// `complexity.cyclomatic p95 [severity=critical|high]` — how the selection reads in a message.
    pub fn describe(&self) -> String {
        let mut s = self.id.clone();
        if let Some(st) = self.stat {
            s.push(' ');
            s.push_str(st.key());
        }
        if !self.labels.is_empty() {
            let l: Vec<String> = self.labels.iter().map(|(k, v)| format!("{k}={}", v.join("|"))).collect();
            s.push_str(&format!(" [{}]", l.join(", ")));
        }
        s
    }

    fn matches(&self, row: &Row, labeled: bool) -> bool {
        row.id == self.id
            && row.scope.level == "target"
            && self.tool.as_deref().is_none_or(|t| row.tool == t)
            && if labeled {
                self.labels.iter().all(|(k, ok)| row.labels.get(k).is_some_and(|v| ok.contains(v)))
            } else {
                row.labels.is_empty()
            }
    }
}

/// The selected metric's value from each tool that reports it — or WHY there is none.
///
/// Fail closed: a missing value is an `Err` with the report's reason. With values present but an
/// analyzer of the metric's family FAILED, it is still an `Err` unless the judge names its `tool`:
/// the report does not say which analyzer measures which metric, so a failed one may be the one
/// whose value is missing, and a fault is never a reason to pass.
pub fn select(report: &Report, sel: &Selector) -> Result<Vec<ToolValue>, String> {
    let fam = family(&sel.id);
    let what = sel.describe();
    if let Some(tool) = &sel.tool {
        let broken = not_ran(report, fam, Some(tool));
        if !broken.is_empty() {
            return Err(format!("`{what}` from {tool}: {}", broken.join("; ")));
        }
    }
    let mut values = per_tool(report, sel, !sel.labels.is_empty())?;
    if values.is_empty() && !sel.labels.is_empty() && sel.stat.is_none() {
        // label rows exist only for labels that occurred: a tool reporting the total counted none.
        values = per_tool(report, sel, false)?.into_iter().map(|t| ToolValue { value: 0.0, ..t }).collect();
    }
    if values.is_empty() {
        let why = not_ran(report, fam, sel.tool.as_deref());
        return Err(if why.is_empty() {
            format!("`{what}` is not in the report — no analyzer of `{fam}` reported it")
        } else {
            format!("`{what}` was not measured — {}", why.join("; "))
        });
    }
    if sel.tool.is_none() {
        let failed: Vec<String> =
            report.analyzers_of(fam, None).filter(|a| a.status == "failed").map(reason).collect();
        if !failed.is_empty() {
            return Err(format!(
                "`{what}`: an analyzer of `{fam}` failed ({}) — it may be the one that measures this; name the tool \
                 with `tool:` to rely on that tool alone",
                failed.join("; ")
            ));
        }
    }
    Ok(values)
}

/// For a `--changed-since` report: the analyzers that had NOTHING TO MEASURE for this selection —
/// `Some(ids)` only when no target row of the metric exists, every analyzer of its family (of its
/// `tool`, if named) was SKIPPED (none ran, none failed), and either no file changed at all
/// (`subject.files == 0`, the structural signal) or each of them gave exactly
/// [`NO_APPLICABLE_FILES`]. Anything else — a failure, a missing tool, a skip for a missing
/// prerequisite, an analyzer that ran but reported no value — is `None`: a real measurement failure.
pub fn nothing_to_measure(report: &Report, sel: &Selector) -> Option<Vec<String>> {
    let subject = report.subject.as_ref()?;
    subject.changed_since.as_ref()?;
    if report.metrics.iter().any(|r| r.id == sel.id && r.scope.level == "target") {
        return None;
    }
    let fam = family(&sel.id);
    let mine: Vec<&Analyzer> = report.analyzers_of(fam, sel.tool.as_deref()).collect();
    // a FAILED sibling fails closed even when the judge names its tool's (skipped) analyzer.
    if mine.is_empty() || report.analyzers_of(fam, None).any(|a| a.status == "failed") {
        return None;
    }
    let skipped = mine.iter().all(|a| a.status == "skipped");
    let none_applicable =
        subject.files == Some(0) || mine.iter().all(|a| a.reason.as_deref() == Some(NO_APPLICABLE_FILES));
    (skipped && none_applicable).then(|| mine.iter().map(|a| a.id.clone()).collect())
}

/// `lizard skipped: no applicable files` for each analyzer of `family` (and `tool`) that did not run.
fn not_ran(report: &Report, family: &str, tool: Option<&str>) -> Vec<String> {
    report.analyzers_of(family, tool).filter(|a| a.status != "ran").map(reason).collect()
}

fn reason(a: &Analyzer) -> String {
    format!("{} {}: {}", a.id, a.status, a.reason.as_deref().unwrap_or("no reason given"))
}

/// The matching rows, one number per tool (summed across label rows).
fn per_tool(report: &Report, sel: &Selector, labeled: bool) -> Result<Vec<ToolValue>, String> {
    let mut by_tool: BTreeMap<String, (Vec<f64>, Better)> = BTreeMap::new();
    for row in report.metrics.iter().filter(|r| sel.matches(r, labeled)) {
        let n = number(row, sel)?;
        by_tool.entry(row.tool.clone()).or_insert_with(|| (Vec::new(), Better::parse(&row.direction))).0.push(n);
    }
    by_tool
        .into_iter()
        .map(|(tool, (ns, better))| {
            if sel.stat.is_some() && ns.len() > 1 {
                return Err(format!(
                    "`{}`: {tool} reports {} rows for these labels — a statistic of each cannot be added up; select one label value",
                    sel.describe(),
                    ns.len()
                ));
            }
            Ok(ToolValue { tool, value: ns.iter().sum(), better })
        })
        .collect()
}

/// The row's number: the selected statistic, or its single value.
fn number(row: &Row, sel: &Selector) -> Result<f64, String> {
    match (sel.stat, row.value, &row.stats) {
        (Some(st), _, Some(stats)) => stats
            .get(st.key())
            .copied()
            .ok_or_else(|| format!("`{}` from {} has no `{}` statistic", sel.id, row.tool, st.key())),
        (Some(st), _, None) => Err(format!(
            "`{}` from {} is a single value, not a distribution — drop `stat: {}`",
            sel.id,
            row.tool,
            st.key()
        )),
        (None, Some(v), _) => Ok(v),
        (None, None, _) => Err(format!(
            "`{}` from {} is a distribution — choose a statistic with `stat:` (count, sum, mean, p50, p95, max)",
            sel.id, row.tool
        )),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A report with these analyzers (`id:tool:status:reason:families`) and rows.
    pub(crate) fn report(analyzers: &[(&str, &str, &str, &[&str])], rows: serde_json::Value) -> Report {
        let a: Vec<serde_json::Value> = analyzers
            .iter()
            .map(|(id, status, reason, fams)| {
                serde_json::json!({"id": id, "tool": id, "status": status, "reason": reason, "families": fams})
            })
            .collect();
        let doc = serde_json::json!({"schema": REPORT_SCHEMA, "analyzers": a, "metrics": rows, "details": []});
        Report::parse(&doc.to_string()).expect("the test report must parse")
    }

    fn sel(id: &str) -> Selector {
        Selector { id: id.into(), tool: None, stat: None, labels: vec![] }
    }

    fn row(id: &str, tool: &str, v: serde_json::Value) -> serde_json::Value {
        let mut r = serde_json::json!({"id": id, "tool": tool, "unit": "x", "direction": "lower_better", "scope": {"level": "target"}});
        for (k, x) in v.as_object().unwrap() {
            r[k] = x.clone();
        }
        r
    }

    const CX: &[&str] = &["complexity"];

    #[test]
    fn a_statistic_and_a_value_are_read_from_target_rows_only() {
        let r = report(
            &[("lizard", "ran", "", CX)],
            serde_json::json!([
                row("complexity.cyclomatic", "lizard", serde_json::json!({"stats": {"p95": 6.0, "max": 9.0}})),
                row("complexity.cyclomatic", "lizard", serde_json::json!({"value": 40.0, "scope": {"level": "function"}})),
            ]),
        );
        let s = Selector { stat: Some(Stat::P95), ..sel("complexity.cyclomatic") };
        assert_eq!(select(&r, &s).unwrap(), [ToolValue { tool: "lizard".into(), value: 6.0, better: Better::Lower }]);
        let e = select(&r, &sel("complexity.cyclomatic")).unwrap_err();
        assert!(e.contains("choose a statistic"), "{e}");
        let e = select(&r, &Selector { stat: Some(Stat::Mean), ..sel("complexity.cyclomatic") }).unwrap_err();
        assert!(e.contains("no `mean` statistic"), "{e}");
    }

    #[test]
    fn labels_sum_their_rows_and_an_absent_label_with_a_total_is_zero() {
        let d: &[&str] = &["dependencies"];
        let vulns = |rows| report(&[("trivy", "ran", "", d)], rows);
        let r = vulns(serde_json::json!([
            row("dependencies.vulnerabilities", "trivy", serde_json::json!({"value": 5.0})),
            row("dependencies.vulnerabilities", "trivy", serde_json::json!({"value": 2.0, "labels": {"severity": "critical"}})),
            row("dependencies.vulnerabilities", "trivy", serde_json::json!({"value": 1.0, "labels": {"severity": "high"}})),
            row("dependencies.vulnerabilities", "trivy", serde_json::json!({"value": 2.0, "labels": {"severity": "low"}})),
        ]));
        let s = Selector { labels: vec![("severity".into(), vec!["critical".into(), "high".into()])], ..sel("dependencies.vulnerabilities") };
        assert_eq!(select(&r, &s).unwrap()[0].value, 3.0, "critical + high");
        assert_eq!(select(&r, &sel("dependencies.vulnerabilities")).unwrap()[0].value, 5.0, "no labels = the total");
        let clean = vulns(serde_json::json!([row("dependencies.vulnerabilities", "trivy", serde_json::json!({"value": 0.0}))]));
        assert_eq!(select(&clean, &s).unwrap()[0].value, 0.0, "none occurred, and the total says so");
        let s2 = Selector { stat: Some(Stat::Max), ..s };
        assert!(select(&r, &s2).is_err(), "a value metric has no statistics");
    }

    /// THE fail-closed rule: a skipped or failed analyzer, or an absent metric, is an error naming
    /// the analyzer's reason — never a pass.
    #[test]
    fn a_missing_value_is_an_error_with_the_analyzers_reason() {
        let r = report(&[("trivy", "skipped", "no applicable files", &["dependencies"])], serde_json::json!([]));
        let e = select(&r, &sel("dependencies.vulnerabilities")).unwrap_err();
        assert!(e.contains("trivy skipped: no applicable files"), "{e}");
        let r = report(&[("jscpd", "failed", "jscpd is not installed", &["duplication"])], serde_json::json!([]));
        let e = select(&r, &sel("duplication.percent")).unwrap_err();
        assert!(e.contains("jscpd failed: jscpd is not installed"), "{e}");
        let r = report(&[("lizard", "ran", "", CX)], serde_json::json!([]));
        let e = select(&r, &sel("complexity.cyclomatic")).unwrap_err();
        assert!(e.contains("not in the report"), "{e}");
    }

    /// A FAILED sibling analyzer fails a judge that does not name its tool; naming the tool scopes
    /// the judge to that tool — and that tool's own skip is then the error.
    #[test]
    fn a_failed_sibling_analyzer_fails_closed_unless_the_tool_is_named() {
        let rows = serde_json::json!([row("complexity.cognitive", "lizard", serde_json::json!({"stats": {"p95": 4.0}}))]);
        let r = report(&[("lizard", "ran", "", CX), ("complexipy", "failed", "complexipy is not installed", CX)], rows.clone());
        let s = Selector { stat: Some(Stat::P95), ..sel("complexity.cognitive") };
        let e = select(&r, &s).unwrap_err();
        assert!(e.contains("complexipy failed") && e.contains("tool:"), "{e}");
        let named = Selector { tool: Some("lizard".into()), ..s.clone() };
        assert_eq!(select(&r, &named).unwrap()[0].value, 4.0);
        let gone = Selector { tool: Some("complexipy".into()), ..s };
        assert!(select(&r, &gone).unwrap_err().contains("complexipy is not installed"));
        // a SKIPPED sibling (not applicable) does not fail a judge whose value is there.
        let r = report(&[("lizard", "ran", "", CX), ("complexipy", "skipped", "no applicable files", CX)], rows);
        assert!(select(&r, &Selector { stat: Some(Stat::P95), ..sel("complexity.cognitive") }).is_ok());
    }

    #[test]
    fn several_label_rows_of_a_statistic_are_refused_and_tools_are_kept_apart() {
        let rows = serde_json::json!([
            row("structure.efferent_coupling", "a", serde_json::json!({"stats": {"p95": 1.0}, "labels": {"language": "python"}})),
            row("structure.efferent_coupling", "a", serde_json::json!({"stats": {"p95": 2.0}, "labels": {"language": "java"}})),
            row("structure.efferent_coupling", "b", serde_json::json!({"stats": {"p95": 3.0}, "labels": {"language": "python"}})),
        ]);
        let r = report(&[("a", "ran", "", &["structure"]), ("b", "ran", "", &["structure"])], rows);
        let both = Selector {
            stat: Some(Stat::P95),
            labels: vec![("language".into(), vec!["python".into(), "java".into()])],
            ..sel("structure.efferent_coupling")
        };
        assert!(select(&r, &both).unwrap_err().contains("cannot be added up"));
        let py = Selector { labels: vec![("language".into(), vec!["python".into()])], ..both };
        let v = select(&r, &py).unwrap();
        assert_eq!(v.iter().map(|t| (t.tool.as_str(), t.value)).collect::<Vec<_>>(), [("a", 1.0), ("b", 3.0)]);
    }

    /// A `--changed-since` report in which the metric's analyzers had nothing to measure.
    fn changed(files: u64, analyzers: &[(&str, &str, &str, &[&str])]) -> Report {
        let mut r = report(analyzers, serde_json::json!([]));
        r.subject = Some(Subject { changed_since: Some("main".into()), files: Some(files) });
        r
    }

    /// Nothing to measure in the changed set — nothing changed, or only files no analyzer of the
    /// family applies to — is told apart from every real failure.
    #[test]
    fn nothing_to_measure_is_only_no_applicable_changed_files() {
        let s = Selector { stat: Some(Stat::P95), ..sel("complexity.cyclomatic") };
        let none = |r: &Report| nothing_to_measure(r, &s);
        let skip = |id| (id, "skipped", NO_APPLICABLE_FILES, CX);
        assert_eq!(none(&changed(0, &[skip("lizard"), skip("complexipy")])), Some(vec!["lizard".into(), "complexipy".into()]));
        assert_eq!(none(&changed(3, &[skip("lizard")])), Some(vec!["lizard".into()]), "only docs changed");
        // nothing changed at all: the structural signal holds whatever the reason text.
        assert!(none(&changed(0, &[("lizard", "skipped", "worded differently", CX)])).is_some());
        // …but with files changed, only the runner's exact reason counts.
        assert_eq!(none(&changed(3, &[("lizard", "skipped", "no SAST rules given", CX)])), None);
        assert_eq!(none(&changed(3, &[("lizard", "skipped", "No applicable files.", CX)])), None, "exact, not fuzzy");
        assert_eq!(none(&changed(0, &[skip("lizard"), ("complexipy", "failed", "complexipy is not installed", CX)])), None);
        assert_eq!(none(&changed(3, &[("lizard", "ran", "", CX)])), None, "it ran and reported nothing: absent");
        assert_eq!(none(&changed(0, &[])), None, "no analyzer of the family at all");
        let mut whole = changed(0, &[skip("lizard")]);
        whole.subject.as_mut().unwrap().changed_since = None;
        assert_eq!(none(&whole), None, "only a changed-scope measurement can be vacuous");
        let named = Selector { tool: Some("lizard".into()), ..s.clone() };
        assert!(nothing_to_measure(&changed(0, &[skip("lizard"), ("complexipy", "failed", "boom", CX)]), &named).is_none());
    }

    #[test]
    fn another_schema_version_is_refused_and_details_follow_their_family() {
        assert!(Report::parse(r#"{"schema":"swe-metrics/report/v2"}"#).unwrap_err().contains("v2"));
        assert!(Report::parse("nope").is_err());
        let doc = serde_json::json!({"schema": REPORT_SCHEMA,
            "analyzers": [{"id": "gitleaks", "tool": "gitleaks", "status": "ran", "families": ["secrets"]},
                          {"id": "jscpd", "tool": "jscpd", "status": "ran", "families": ["duplication"]}],
            "metrics": [], "details": [{"analyzer": "gitleaks", "format": "sarif", "path": "gitleaks.sarif"},
                                        {"analyzer": "jscpd", "format": "jscpd-json", "path": "jscpd.json"}]});
        let r = Report::parse(&doc.to_string()).unwrap();
        assert_eq!(r.details_of(Some("secrets")).iter().map(|d| d.path.as_str()).collect::<Vec<_>>(), ["gitleaks.sarif"]);
        assert_eq!(r.details_of(None).len(), 2);
        assert_eq!(Better::parse("higher_better").as_str(), "higher_better");
    }
}
