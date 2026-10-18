//! Judges built on **swe-metrics** — a separate, stateless measuring tool that turns a project into a
//! JSON report of metrics (and the tools' SARIF details) and never judges them. agg does the
//! judging: a metric judge holds one number of the report to a threshold, a score judge holds
//! swe-metrics' quality score to one, and both feed the ratchet like any judge with a `value`.
//!
//! ```yaml
//! judges:
//!   cc_p95:  { metric: { id: "complexity.cyclomatic", stat: "p95", tool: "lizard" }, max: 10 }
//!   vulns:   { metric: { id: "dependencies.vulnerabilities", labels: { severity: ["critical", "high"] } }, max: 0 }
//!   quality: { score: { definition: "swe-default", version: "2" }, min: 80 }
//! ```
//!
//! - [`spec`]    the YAML blocks, the runtime judges, their startup shape checks
//! - [`report`]  the report and the selection of a metric's value per tool — fail closed
//! - [`combine`] several tools for one metric: `strictest` · `all` · `majority`
//! - [`measure`] ONE `swe-metrics measure` per session tree, shared; the incumbent's report
//! - [`score`]   `swe-metrics score` against the incumbent, and its verdict
//! - [`probe`]   the startup refusal when swe-metrics is missing or incompatible — on the host, or
//!   in the judges' image when the run's judges are containerized
//! - [`contained`] swe-metrics in the judges' image under `isolation: container`
//! - [`declare`] startup: `judges.*` entries → kinds, ratchet directions, shared `--only` sets
//!
//! swe-metrics is OPTIONAL: nothing here runs unless a judge is declared with `metric:`/`score:`.

pub mod combine;
pub mod contained;
pub mod declare;
pub mod measure;
pub mod probe;
pub mod report;
pub mod score;
pub mod spec;

pub use spec::{Combine, MetricJudge, MetricSel, OneOrMany, Scope, ScoreJudge, ScoreSel, Stat};

use crate::core::model::Verdict;
use crate::core::ratchet::Direction;
use combine::Threshold;
use measure::Ctx;
use report::{Better, Selector};

/// Run a metric judge: the session's shared measurement, the selected value, the threshold.
pub fn run_metric(j: &MetricJudge, c: &Ctx<'_>) -> Verdict {
    match metric_verdict(j, c) {
        Ok(v) => v,
        Err((e, evidence)) => Verdict::failed(e).with_evidence(evidence),
    }
}

fn metric_verdict(j: &MetricJudge, c: &Ctx<'_>) -> Result<Verdict, (String, Vec<String>)> {
    let m = measure::measure(c, &j.scope, j.only.as_deref()).map_err(|e| (e, vec![]))?;
    let evidence = evidence(&m, Some(j.family()), None);
    let sel = selector(j);
    if j.scope.is_changes() {
        if let Some(ids) = report::nothing_to_measure(&m.report, &sel) {
            return Ok(vacuous(&sel, &ids, evidence));
        }
    }
    let fail = |e: String| (e, evidence.clone());
    let values = report::select(&m.report, &sel).map_err(fail)?;
    let incumbent = incumbent_values(j, c, &sel);
    let t = Threshold { max: j.max, min: j.min };
    let d = combine::decide(&j.sel.id, &values, j.sel.combine, t, incumbent.as_deref()).map_err(fail)?;
    check_ratchet(j, d.better).map_err(fail)?;
    let mut why = format!("{} = {} ({}) {} {}", sel.describe(), d.value, d.how, if d.met { "meets" } else { "misses" }, t.describe());
    if d.mixed {
        why.push_str("; the tools moved in different directions since the incumbent — not an improvement");
    }
    let mut v = Verdict::binary(d.met).with_value(d.value).with_rationale(why).with_evidence(evidence);
    v.target = j.max.or(j.min).unwrap_or(1.0);
    v.mixed = d.mixed;
    Ok(v)
}

/// "New code must be clean" with no new code in scope is VACUOUSLY met: no number, and a verdict that
/// moves nothing at the gate (`Verdict::vacuous`).
fn vacuous(sel: &Selector, analyzers: &[String], evidence: Vec<String>) -> Verdict {
    let why = format!("no changed files in scope for {} — `{}` is vacuously met", analyzers.join(", "), sel.describe());
    let mut v = Verdict::binary(true).with_rationale(why).with_evidence(evidence);
    v.vacuous = true;
    v
}

/// The report's direction must agree with the ratchet's: `max:` (down) on a higher-is-better
/// metric would hold the number the wrong way, and a ratchet must never do that silently.
fn check_ratchet(j: &MetricJudge, better: Better) -> Result<(), String> {
    let contradicts = matches!((j.ratchet, better), (Some(Direction::Down), Better::Higher) | (Some(Direction::Up), Better::Lower));
    if contradicts {
        let dir = if j.ratchet == Some(Direction::Down) { "down" } else { "up" };
        return Err(format!(
            "the judge is ratcheted `{dir}`, but the report says `{}` is {} — set the ratchet's `direction` (or the \
             threshold) to match the metric",
            j.sel.id,
            better.as_str()
        ));
    }
    Ok(())
}

/// The same selection on the incumbent's report — only for `combine: all`, and only after the
/// baseline (a pass before the first session has no incumbent of its own run).
fn incumbent_values(j: &MetricJudge, c: &Ctx<'_>, sel: &Selector) -> Option<Vec<report::ToolValue>> {
    if j.sel.combine != Some(Combine::All) || c.session.unwrap_or(0) == 0 {
        return None;
    }
    let text = std::fs::read_to_string(measure::incumbent(c.cwd, &j.scope)?).ok()?;
    report::select(&report::Report::parse(&text).ok()?, sel).ok()
}

fn selector(j: &MetricJudge) -> Selector {
    Selector {
        id: j.sel.id.clone(),
        tool: j.sel.tool.clone(),
        stat: j.sel.stat,
        labels: j.sel.labels.iter().map(|(k, v)| (k.clone(), v.values())).collect(),
    }
}

/// The evidence a verdict cites, relative to the judge scratch: the report, the details of the
/// metric's family (all of them for a score), and `extra`.
fn evidence(m: &measure::Measured, family: Option<&str>, extra: Option<&str>) -> Vec<String> {
    let mut out = vec![format!("{}/report.json", m.rel)];
    out.extend(m.report.details_of(family).iter().map(|d| format!("{}/details/{}", m.rel, d.path)));
    out.extend(extra.map(String::from));
    out
}

/// Run a score judge: its scope's shared report, scored against the incumbent's.
pub fn run_score(name: &str, j: &ScoreJudge, c: &Ctx<'_>) -> Verdict {
    if j.scope == Scope::Changed {
        return Verdict::failed("a score is compared against the incumbent of the same files — `changed` measures other files every session");
    }
    let m = match measure::measure(c, &j.scope, None) {
        Ok(m) => m,
        Err(e) => return Verdict::failed(e),
    };
    let file = format!("score-{}.json", crate::core::evidence::dir_name(name));
    let evidence = evidence(&m, None, Some(&format!("{}/{file}", m.rel)));
    // the incumbent is the baseline from session 1 on; the pass before it measures the incumbent.
    if j.scope.is_changes() && m.report.subject.as_ref().and_then(|s| s.files) == Some(0) {
        let mut v = Verdict::binary(true).with_rationale("no changed files in scope — the score is vacuously met").with_evidence(evidence);
        v.vacuous = true;
        return v;
    }
    // changed files have no incumbent of the same files to compare with.
    let baseline = if c.session.unwrap_or(0) == 0 || j.scope.is_changes() { None } else { measure::incumbent(c.cwd, &j.scope) };
    let concluded = score::run(c, j, &m.report_path(), baseline.as_deref(), &m.dir.join(&file))
        .and_then(|doc| score::conclude(&doc, j));
    match concluded {
        Ok((value, met, why)) => {
            let mut v = Verdict::binary(met).with_value(value).with_rationale(why).with_evidence(evidence);
            v.target = j.min.or(j.max).unwrap_or(1.0);
            v
        }
        Err(e) => Verdict::failed(e).with_evidence(evidence),
    }
}
