//! The `metric:` / `score:` blocks of a `judges.<name>` entry as written in `agg.yaml`, the runtime
//! judges they become, and the startup checks of their SHAPE (the probe of swe-metrics itself is
//! [`super::probe`]).

use crate::core::ratchet::Direction;
use crate::problem::Problem;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `judges.<name>.metric` — which number of the swe-metrics report the judge reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricSel {
    /// the metric ID, `family.name` — `complexity.cyclomatic`, `duplication.percent`.
    pub id: String,
    /// read this tool's value only (`lizard`). Alternative to `combine`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// when several tools report the metric: `strictest` (the worst value), `all` (every tool
    /// within the threshold; tools moving in different directions are no improvement for a
    /// ratchet) or `majority` (more than half of them). Without it, several tools is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combine: Option<Combine>,
    /// the statistic of a distribution metric: `count`, `sum`, `mean`, `p50`, `p95`, `max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stat: Option<Stat>,
    /// only rows with these labels, each a value or a list of accepted values; the matching rows
    /// are summed (`severity: ["critical", "high"]`). Without it, the unlabeled total.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, OneOrMany>,
    /// what is measured: `target` (the project, the default), `changed` (only the files changed
    /// since the base branch) or a project-relative sub-path (a directory or file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// A label's accepted value(s).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    /// The accepted values as a list.
    pub fn values(&self) -> Vec<String> {
        match self {
            OneOrMany::One(v) => vec![v.clone()],
            OneOrMany::Many(v) => v.clone(),
        }
    }
}

/// How the values of several tools for one metric become one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Combine {
    Strictest,
    All,
    Majority,
}

/// A statistic of a distribution metric (the report's `stats`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Stat {
    Count,
    Sum,
    Mean,
    P50,
    P95,
    Max,
}

impl Stat {
    /// The key in the report's `stats` object.
    pub fn key(self) -> &'static str {
        match self {
            Stat::Count => "count",
            Stat::Sum => "sum",
            Stat::Mean => "mean",
            Stat::P50 => "p50",
            Stat::P95 => "p95",
            Stat::Max => "max",
        }
    }
}

/// `judges.<name>.score` — which scoring definition, at which version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScoreSel {
    /// `swe-default` (swe-metrics' shipped one), a `.py` file (relative to `agg/`, or absolute)
    /// or `package.module:NAME`.
    pub definition: String,
    /// the version the definition must report; a score of any other version is refused.
    pub version: String,
    /// what is scored: `target` (the project) or a project-relative sub-path, measured as its own
    /// target. Not `changed`: a score is compared against the incumbent of the same files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// What one measurement covers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// the whole project.
    Target,
    /// only the files changed since the base branch (`--changed-since`).
    Changed,
    /// a project-relative sub-path, measured as its own target.
    Path(String),
    /// only the files of a project-relative sub-path changed since a Git ref (`--changed-since`): what
    /// one workflow step wrote, from the ref it began at. The driver's alone; YAML has no form of it.
    ChangedIn { path: String, since: String },
}

impl Scope {
    /// Parse a `scope:` value; `Err` says why it cannot be one.
    pub fn parse(s: Option<&str>) -> Result<Scope, String> {
        match s.map(str::trim) {
            None | Some("target") => Ok(Scope::Target),
            Some("changed") => Ok(Scope::Changed),
            Some("") => Err("`scope` is empty — use `target`, `changed` or a project-relative path".into()),
            Some(p) => {
                let path = Path::new(p);
                if p.contains(['*', '?', '[', '{']) {
                    return Err(format!(
                        "`scope: {p}` is a glob — swe-metrics measures a file or directory (its statistics are per measured \
                         target), so name the directory to measure"
                    ));
                }
                if path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                    return Err(format!("`scope: {p}` must be a path inside the project (relative, no `..`)"));
                }
                Ok(Scope::Path(p.trim_end_matches('/').to_string()))
            }
        }
    }

    /// A filesystem-safe name for this scope's measurement directory.
    pub fn dir_name(&self) -> String {
        match self {
            Scope::Target => "target".into(),
            Scope::Changed => "changed".into(),
            Scope::Path(p) => format!("path-{}-{}", safe(p), &crate::protect::sha256::hex(p.as_bytes())[..8]),
            Scope::ChangedIn { path, since } => {
                format!("changed-{}-{}", safe(path), &crate::protect::sha256::hex(format!("{path}\0{since}").as_bytes())[..8])
            }
        }
    }

    /// Whether only changed files are measured: with none, a judge is vacuously met.
    pub fn is_changes(&self) -> bool {
        matches!(self, Scope::Changed | Scope::ChangedIn { .. })
    }
}

/// `p` with every character but letters, digits and `-` as `_`.
fn safe(p: &str) -> String {
    p.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// A scoring definition, resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Definition {
    /// swe-metrics' shipped default (its `score` takes no `--definition` for it).
    Default,
    /// a definition file, absolute.
    File(PathBuf),
    /// `package.module:NAME`.
    Module(String),
}

/// The name swe-metrics gives its shipped definition.
pub const DEFAULT_DEFINITION: &str = "swe-default";

impl Definition {
    /// Resolve a `definition:` value; a file is relative to `config_base` (the `agg/` folder).
    pub fn parse(s: &str, config_base: &Path) -> Definition {
        if s == DEFAULT_DEFINITION {
            Definition::Default
        } else if s.ends_with(".py") || s.contains('/') {
            let p = Path::new(s);
            Definition::File(if p.is_absolute() { p.to_path_buf() } else { config_base.join(p) })
        } else {
            Definition::Module(s.to_string())
        }
    }

    /// The `--definition` arguments for `swe-metrics score`.
    pub fn args(&self) -> Vec<String> {
        match self {
            Definition::Default => vec![],
            Definition::File(p) => vec!["--definition".into(), p.display().to_string()],
            Definition::Module(m) => vec!["--definition".into(), m.clone()],
        }
    }
}

/// A METRIC judge, ready to run: what to read, the threshold, and what the measurement covers.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricJudge {
    pub sel: MetricSel,
    pub scope: Scope,
    pub max: Option<f64>,
    pub min: Option<f64>,
    /// the direction the judge is ratcheted in, if it is — checked against the metric's own
    /// direction in the report, so a ratchet never holds a number the wrong way.
    pub ratchet: Option<Direction>,
    /// the metric families this scope's ONE shared measurement covers (`--only`); `None` = all.
    /// Every judge of a scope carries the same set, which is what makes them share it.
    pub only: Option<Vec<String>>,
}

impl MetricJudge {
    /// A metric judge on `id`, over the whole project, with no threshold yet (the driver builder).
    pub fn new(id: impl Into<String>) -> MetricJudge {
        let sel = MetricSel { id: id.into(), tool: None, combine: None, stat: None, labels: BTreeMap::new(), scope: None };
        MetricJudge { sel, scope: Scope::Target, max: None, min: None, ratchet: None, only: None }
    }
    pub fn tool(mut self, t: impl Into<String>) -> Self {
        self.sel.tool = Some(t.into());
        self
    }
    pub fn stat(mut self, s: Stat) -> Self {
        self.sel.stat = Some(s);
        self
    }
    pub fn combine(mut self, c: Combine) -> Self {
        self.sel.combine = Some(c);
        self
    }
    pub fn label(mut self, key: impl Into<String>, values: &[&str]) -> Self {
        self.sel.labels.insert(key.into(), OneOrMany::Many(values.iter().map(|v| v.to_string()).collect()));
        self
    }
    pub fn scope(mut self, s: Scope) -> Self {
        self.scope = s;
        self
    }
    pub fn max(mut self, m: f64) -> Self {
        self.max = Some(m);
        self
    }
    pub fn min(mut self, m: f64) -> Self {
        self.min = Some(m);
        self
    }

    /// The family of the metric: the part of its ID before the dot.
    pub fn family(&self) -> &str {
        family(&self.sel.id)
    }
}

/// The family of a metric ID.
pub fn family(id: &str) -> &str {
    id.split('.').next().unwrap_or(id)
}

/// A SCORE judge, ready to run.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreJudge {
    pub definition: Definition,
    /// the definition as configured — part of the score's comparability basis.
    pub definition_name: String,
    pub version: String,
    /// what is scored: never [`Scope::Changed`]; a [`Scope::ChangedIn`] score has no incumbent to compare with.
    pub scope: Scope,
    pub max: Option<f64>,
    pub min: Option<f64>,
}

impl ScoreJudge {
    /// A score judge by `definition` (`swe-default`, a `.py` path or `package.module:NAME`) at
    /// `version` — a relative file is taken relative to the process' working directory here.
    pub fn new(definition: &str, version: &str) -> ScoreJudge {
        ScoreJudge {
            definition: Definition::parse(definition, Path::new(".")),
            definition_name: definition.to_string(),
            version: version.to_string(),
            scope: Scope::Target,
            max: None,
            min: None,
        }
    }
    /// Score only this sub-path (`Scope::Path`), or its files changed since a ref (`Scope::ChangedIn`).
    pub fn scope(mut self, s: Scope) -> Self {
        self.scope = s;
        self
    }
    pub fn max(mut self, m: f64) -> Self {
        self.max = Some(m);
        self
    }
    pub fn min(mut self, m: f64) -> Self {
        self.min = Some(m);
        self
    }

    /// What the score's value is comparable under: two definitions' scores are not.
    pub fn basis(&self) -> String {
        format!("{}@{}", self.definition_name, self.version)
    }
}

/// The metric ID shape the report schema states (`^[a-z_]+\.[a-z_]+$`).
fn valid_id(id: &str) -> bool {
    let mut parts = id.split('.');
    let ok = |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// The declared parts of one `judges.<name>` entry.
pub struct Declared<'a> {
    pub name: &'a str,
    pub metric: Option<&'a MetricSel>,
    pub score: Option<&'a ScoreSel>,
    pub max: Option<f64>,
    pub min: Option<f64>,
}

/// Every problem with the SHAPE of one `judges.<name>` entry, each at its own key.
pub fn problems(d: &Declared<'_>, config_base: &Path) -> Vec<Problem> {
    let at = |k: &str| format!("judges.{}{k}", d.name);
    let mut out = Vec::new();
    if d.metric.is_some() && d.score.is_some() {
        out.push(Problem::new(at(""), "a judge is a `metric:` OR a `score:` judge, not both"));
    }
    if d.metric.is_none() && d.score.is_none() {
        if d.max.is_some() || d.min.is_some() {
            out.push(Problem::new(at(""), "`max:`/`min:` are the threshold of a `metric:` or `score:` judge — this entry declares neither"));
        }
        return out;
    }
    out.extend(threshold_problems(d, &at));
    if let Some(m) = d.metric {
        out.extend(metric_problems(m, &at));
    }
    if let Some(s) = d.score {
        out.extend(score_problems(s, config_base, &at));
    }
    out
}

fn threshold_problems(d: &Declared<'_>, at: &dyn Fn(&str) -> String) -> Vec<Problem> {
    let mut out = Vec::new();
    match (d.max, d.min) {
        (None, None) => out.push(Problem::new(
            at(""),
            "a metric/score judge needs a threshold — `max:` (met when the value is at most it) and/or `min:` (at least it)",
        )),
        (Some(hi), Some(lo)) if hi < lo => {
            out.push(Problem::new(at(".max"), format!("`max: {hi}` is below `min: {lo}` — nothing could ever be met")))
        }
        _ => {}
    }
    for (k, v) in [("max", d.max), ("min", d.min)] {
        if v.is_some_and(|v| !v.is_finite()) {
            out.push(Problem::new(at(&format!(".{k}")), format!("`{k}` must be a finite number")));
        }
    }
    out
}

fn metric_problems(m: &MetricSel, at: &dyn Fn(&str) -> String) -> Vec<Problem> {
    let mut out = Vec::new();
    if !valid_id(&m.id) {
        out.push(Problem::new(at(".metric.id"), format!("`{}` is not a metric ID — `family.name`, lowercase (`complexity.cyclomatic`)", m.id)));
    }
    if m.tool.is_some() && m.combine.is_some() {
        out.push(Problem::new(at(".metric.combine"), "`tool:` picks ONE tool and `combine:` merges several — set one of them"));
    }
    if m.tool.as_deref().is_some_and(|t| t.trim().is_empty()) {
        out.push(Problem::new(at(".metric.tool"), "`tool` is empty"));
    }
    for (k, v) in &m.labels {
        if v.values().is_empty() || v.values().iter().any(|x| x.is_empty()) {
            out.push(Problem::new(at(&format!(".metric.labels.{k}")), "a label needs at least one non-empty value"));
        }
    }
    if let Err(e) = Scope::parse(m.scope.as_deref()) {
        out.push(Problem::new(at(".metric.scope"), e));
    }
    out
}

fn score_problems(s: &ScoreSel, config_base: &Path, at: &dyn Fn(&str) -> String) -> Vec<Problem> {
    let mut out = Vec::new();
    if s.version.trim().is_empty() {
        out.push(Problem::new(at(".score.version"), "`version` is empty — name the definition version you compare under"));
    }
    match Definition::parse(&s.definition, config_base) {
        Definition::File(p) if !p.is_file() => out.push(Problem::new(
            at(".score.definition"),
            format!("scoring definition file {} does not exist (a relative path is taken from agg/)", p.display()),
        )),
        Definition::Module(m) if !m.contains(':') => out.push(Problem::new(
            at(".score.definition"),
            format!("`{m}` is neither `{DEFAULT_DEFINITION}`, a `.py` file nor `package.module:NAME`"),
        )),
        _ => {}
    }
    match Scope::parse(s.scope.as_deref()) {
        Err(e) => out.push(Problem::new(at(".score.scope"), e)),
        Ok(Scope::Changed) => out.push(Problem::new(
            at(".score.scope"),
            "a score is compared against the incumbent of the same files — `changed` measures other files every session; \
             use `target` or a sub-path",
        )),
        Ok(_) => {}
    }
    out
}

/// The runtime judge a VALID metric declaration becomes (shape problems were refused already).
pub fn metric_judge(m: &MetricSel, max: Option<f64>, min: Option<f64>) -> MetricJudge {
    let scope = Scope::parse(m.scope.as_deref()).unwrap_or(Scope::Target);
    MetricJudge { sel: m.clone(), scope, max, min, ratchet: None, only: None }
}

/// The runtime judge a VALID score declaration becomes.
pub fn score_judge(s: &ScoreSel, max: Option<f64>, min: Option<f64>, config_base: &Path) -> ScoreJudge {
    ScoreJudge {
        definition: Definition::parse(&s.definition, config_base),
        definition_name: s.definition.clone(),
        version: s.version.clone(),
        scope: Scope::parse(s.scope.as_deref()).unwrap_or(Scope::Target),
        max,
        min,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(yaml: &str) -> MetricSel {
        serde_yaml::from_str(yaml).expect("the selector must parse")
    }

    fn paths(d: &Declared<'_>) -> Vec<String> {
        problems(d, Path::new("/nowhere")).into_iter().map(|p| p.path).collect()
    }

    /// The shapes the requirement writes parse as written, labels as a value or a list.
    #[test]
    fn the_documented_selectors_parse() {
        let m = sel("{ id: complexity.cyclomatic, stat: p95, tool: lizard }");
        assert_eq!((m.stat, m.tool.as_deref()), (Some(Stat::P95), Some("lizard")));
        let v = sel("{ id: dependencies.vulnerabilities, labels: { severity: [critical, high] } }");
        assert_eq!(v.labels["severity"].values(), ["critical", "high"]);
        let one = sel("{ id: security.findings, labels: { level: error }, combine: all, scope: changed }");
        assert_eq!(one.labels["level"].values(), ["error"]);
        assert_eq!(one.combine, Some(Combine::All));
        assert!(serde_yaml::from_str::<MetricSel>("{ id: a.b, bogus: 1 }").is_err(), "deny_unknown_fields");
        assert!(serde_yaml::from_str::<MetricSel>("{ id: a.b, stat: p99 }").is_err(), "an unknown statistic");
    }

    /// Each shape problem is refused at the key to edit.
    #[test]
    fn shape_problems_are_located() {
        let both = sel("{ id: a.b, tool: x, combine: all }");
        let score = ScoreSel { definition: DEFAULT_DEFINITION.into(), version: "2".into(), scope: None };
        let d = |metric, score, max, min| Declared { name: "j", metric, score, max, min };
        assert_eq!(paths(&d(Some(&both), Some(&score), Some(1.0), None)), ["judges.j", "judges.j.metric.combine"]);
        let ab = sel("{ id: a.b }");
        assert_eq!(paths(&d(Some(&ab), None, None, None)), ["judges.j"], "no threshold");
        assert_eq!(paths(&d(Some(&ab), None, Some(1.0), Some(2.0))), ["judges.j.max"]);
        let upper = sel("{ id: A.b }");
        assert_eq!(paths(&d(Some(&upper), None, Some(1.0), None)), ["judges.j.metric.id"]);
        let glob = sel("{ id: a.b, scope: 'src/*' }");
        assert_eq!(paths(&d(Some(&glob), None, Some(1.0), None)), ["judges.j.metric.scope"]);
        let nolabel = sel("{ id: a.b, labels: { k: [] } }");
        assert_eq!(paths(&d(Some(&nolabel), None, Some(1.0), None)), ["judges.j.metric.labels.k"]);
        assert_eq!(paths(&d(None, None, Some(1.0), None)), ["judges.j"], "a threshold without a judge");
        assert!(paths(&d(None, None, None, None)).is_empty(), "a plain timeout entry is fine");
        let empty = ScoreSel { definition: "nope".into(), version: " ".into(), scope: None };
        assert_eq!(paths(&d(None, Some(&empty), None, Some(80.0))), ["judges.j.score.version", "judges.j.score.definition"]);
        let file = ScoreSel { definition: "scoring.py".into(), version: "1".into(), scope: None };
        assert_eq!(paths(&d(None, Some(&file), None, Some(80.0))), ["judges.j.score.definition"], "a missing file");
        let changed = ScoreSel { scope: Some("changed".into()), ..score.clone() };
        assert_eq!(paths(&d(None, Some(&changed), None, Some(80.0))), ["judges.j.score.scope"], "no incumbent of the same files");
        let work = ScoreSel { scope: Some("work".into()), ..score.clone() };
        assert!(paths(&d(None, Some(&work), None, Some(80.0))).is_empty());
        assert_eq!(score_judge(&work, None, Some(80.0), Path::new("/x")).scope, Scope::Path("work".into()));
    }

    /// A scope is the project, its changes, or a sub-path — never a glob, never outside.
    #[test]
    fn scopes_parse_and_name_their_directory() {
        assert_eq!(Scope::parse(None), Ok(Scope::Target));
        assert_eq!(Scope::parse(Some("changed")), Ok(Scope::Changed));
        assert_eq!(Scope::parse(Some("src/core/")), Ok(Scope::Path("src/core".into())));
        for bad in ["", "../x", "/abs", "src/**"] {
            assert!(Scope::parse(Some(bad)).is_err(), "{bad}");
        }
        assert_eq!(Scope::Target.dir_name(), "target");
        let since = |r: &str| Scope::ChangedIn { path: "work".into(), since: r.into() };
        assert!(since("a").dir_name().starts_with("changed-work-") && since("a").dir_name() != since("b").dir_name());
        assert!(since("a").is_changes() && Scope::Changed.is_changes() && !Scope::Path("work".into()).is_changes());
        let d = Scope::Path("src/core".into()).dir_name();
        assert!(d.starts_with("path-src_core-") && d.len() == "path-src_core-".len() + 8, "{d}");
    }

    /// A definition is the shipped default, a file under agg/, or a module path.
    #[test]
    fn definitions_resolve() {
        let base = Path::new("/p/agg");
        assert_eq!(Definition::parse("swe-default", base), Definition::Default);
        assert_eq!(Definition::parse("scoring.py", base), Definition::File("/p/agg/scoring.py".into()));
        assert_eq!(Definition::parse("/abs/s.py", base), Definition::File("/abs/s.py".into()));
        assert_eq!(Definition::parse("team.scoring:DEF", base), Definition::Module("team.scoring:DEF".into()));
        assert!(Definition::Default.args().is_empty(), "swe-metrics refuses `--definition swe-default`");
        assert_eq!(ScoreJudge::new("swe-default", "2").basis(), "swe-default@2");
    }

    #[test]
    fn the_driver_builder_sets_what_it_says() {
        let m = MetricJudge::new("dependencies.vulnerabilities").label("severity", &["high"]).combine(Combine::All).max(0.0);
        assert_eq!((m.family(), m.max, m.sel.combine), ("dependencies", Some(0.0), Some(Combine::All)));
        let m = MetricJudge::new("x.y").tool("t").stat(Stat::Max).scope(Scope::Changed).min(1.0);
        assert_eq!((m.sel.tool.as_deref(), m.sel.stat.map(Stat::key), m.scope, m.min), (Some("t"), Some("max"), Scope::Changed, Some(1.0)));
        let s = ScoreJudge::new("swe-default", "2").max(100.0).min(80.0);
        assert_eq!((s.max, s.min), (Some(100.0), Some(80.0)));
    }
}
