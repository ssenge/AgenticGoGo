//! Startup: the `judges.<name>` entries that DECLARE a metric or score judge become runnable kinds,
//! the ratchets that leave their `direction` to such a judge are resolved, the judges of one scope
//! are given one shared `--only` set, and swe-metrics is probed where the run will run it (the host,
//! or the judges' image of a container run) — every problem located.

use super::spec::{self, Declared as Shape, MetricJudge, Scope};
use crate::core::config::AggConfig;
use crate::core::model::JudgeKind;
use crate::core::ratchet::{Direction, Ratchet};
use crate::problem::Problem;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What [`declare`] resolved.
#[derive(Debug, Default)]
pub struct Declared {
    /// every judge declared by a `metric:`/`score:` block, by name.
    pub kinds: BTreeMap<String, JudgeKind>,
    /// every ratchet whose direction is known — written, or stated by its metric/score judge.
    pub ratchets: BTreeMap<String, Ratchet>,
    /// the comparability basis of each score judge (`definition@version`).
    pub bases: BTreeMap<String, String>,
    pub problems: Vec<Problem>,
}

/// Resolve the declared judges of `cfg`. `run_set` = the judge names the run executes: only those
/// shape the shared measurements and are probed for. `probe` = whether to call swe-metrics.
pub fn declare(cfg: &AggConfig, run_set: &[String], config_base: &Path, probe: bool) -> Declared {
    let mut d = Declared::default();
    for (name, o) in &cfg.judges {
        let shape = Shape { name, metric: o.metric.as_ref(), score: o.score.as_ref(), max: o.max, min: o.min };
        let found = spec::problems(&shape, config_base);
        let declared = o.metric.is_some() || o.score.is_some();
        if declared && found.is_empty() {
            if let Some(file) = judge_file(name, config_base) {
                d.problems.push(Problem::new(
                    format!("judges.{name}"),
                    format!("`{name}` is declared here AND as {} — one judge, one definition", file.display()),
                ));
            }
            let kind = match (&o.metric, &o.score) {
                (Some(m), _) => JudgeKind::Metric(Box::new(spec::metric_judge(m, o.max, o.min))),
                (_, Some(s)) => JudgeKind::Score(Box::new(spec::score_judge(s, o.max, o.min, config_base))),
                _ => unreachable!("declared"),
            };
            d.kinds.insert(name.clone(), kind);
        }
        d.problems.extend(found);
    }
    resolve_ratchets(cfg, &mut d);
    share_measurements(run_set, &mut d);
    for (name, k) in &d.kinds {
        if let JudgeKind::Score(s) = k {
            d.bases.insert(name.clone(), s.basis());
        }
    }
    if probe {
        // where the run's swe-metrics will run: the host, or the judges' image of a container run.
        let project = config_base.parent().unwrap_or(config_base);
        if let Some(site) = super::probe::Site::of(cfg, project) {
            d.problems.extend(probe_run_set(run_set, &d.kinds, &site));
        }
    }
    d
}

/// The project's own judge file for `name`, if there is one.
fn judge_file(name: &str, config_base: &Path) -> Option<std::path::PathBuf> {
    let dir = crate::core::judges::project_judges_dir(config_base);
    ["sh", "md"].iter().map(|e| dir.join(format!("{name}.{e}"))).find(|p| p.is_file())
}

/// The direction a judge states for itself: a score is higher-better; a metric judge's threshold
/// says it (`max:` ⇒ down, `min:` ⇒ up — and the report's direction is checked against it at
/// every run). `None` = it states none.
fn implied(kind: Option<&JudgeKind>) -> Option<Direction> {
    match kind {
        Some(JudgeKind::Score(_)) => Some(Direction::Up),
        Some(JudgeKind::Metric(m)) => match (m.max, m.min) {
            (Some(_), None) => Some(Direction::Down),
            (None, Some(_)) => Some(Direction::Up),
            _ => None,
        },
        _ => None,
    }
}

fn resolve_ratchets(cfg: &AggConfig, d: &mut Declared) {
    for (name, rc) in &cfg.sequence.ratchet {
        let at = format!("sequence.ratchet.{name}");
        let kind = d.kinds.get(name);
        if matches!(kind, Some(JudgeKind::Score(_))) && rc.direction == Some(Direction::Down) {
            d.problems.push(Problem::new(format!("{at}.direction"), "a score is higher-is-better — its ratchet direction is `up`"));
            continue;
        }
        if let Some(JudgeKind::Metric(m)) = kind {
            if m.scope == Scope::Changed {
                d.problems.push(Problem::new(
                    at.clone(),
                    "a `scope: changed` judge measures only each session's own changes — ratcheting it would compare \
                     different files every session",
                ));
                continue;
            }
        }
        let Some(r) = rc.resolve(implied(kind)) else {
            let why = match kind {
                Some(JudgeKind::Metric(_)) => "the judge has both `max:` and `min:`, so it states no direction — set `direction: up | down`",
                _ => "`direction` (up | down) is required — only a metric or score judge states its own",
            };
            d.problems.push(Problem::new(format!("{at}.direction"), why));
            continue;
        };
        if let Some(JudgeKind::Metric(m)) = d.kinds.get_mut(name) {
            m.ratchet = Some(r.direction);
        }
        d.ratchets.insert(name.clone(), r);
    }
}

/// Every metric judge of one scope gets the same `--only` set — the union of their families — so
/// they share ONE measurement; a score judge needs every family, so its scope then has none.
fn share_measurements(run_set: &[String], d: &mut Declared) {
    let in_run = |n: &String| run_set.contains(n);
    let scored: BTreeSet<Scope> = d.kinds.iter().filter(|(n, _)| in_run(n))
        .filter_map(|(_, k)| if let JudgeKind::Score(s) = k { Some(s.scope.clone()) } else { None }).collect();
    let mut families: BTreeMap<Scope, BTreeSet<String>> = BTreeMap::new();
    for (_, k) in d.kinds.iter().filter(|(n, _)| in_run(n)) {
        if let JudgeKind::Metric(m) = k {
            families.entry(m.scope.clone()).or_default().insert(m.family().to_string());
        }
    }
    for k in d.kinds.values_mut() {
        if let JudgeKind::Metric(m) = k {
            m.only = only_for(m, &scored, &families);
        }
    }
}

fn only_for(m: &MetricJudge, scored: &BTreeSet<Scope>, families: &BTreeMap<Scope, BTreeSet<String>>) -> Option<Vec<String>> {
    if scored.contains(&m.scope) {
        return None;
    }
    families.get(&m.scope).map(|f| f.iter().cloned().collect())
}

fn probe_run_set(run_set: &[String], kinds: &BTreeMap<String, JudgeKind>, site: &super::probe::Site) -> Vec<Problem> {
    let mut metric: Vec<&str> = Vec::new();
    let mut score: Vec<(&str, &spec::ScoreJudge)> = Vec::new();
    // trivy measures the `dependencies` family, and a score reads every family.
    let mut trivy = false;
    for (n, k) in kinds.iter().filter(|(n, _)| run_set.contains(n)) {
        match k {
            JudgeKind::Metric(m) => {
                trivy |= m.family() == "dependencies";
                metric.push(n)
            }
            JudgeKind::Score(s) => {
                trivy = true;
                score.push((n, s))
            }
            _ => {}
        }
    }
    super::probe::problems(&metric, &score, site, trivy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(judges: &str, ratchet: &str) -> AggConfig {
        let ratchet = if ratchet.is_empty() { " {}\n".to_string() } else { format!("\n{ratchet}") };
        let body = format!("project: p\njudges:\n{judges}sequence:\n  ratchet:{ratchet}");
        serde_yaml::from_str(&body).unwrap_or_else(|e| panic!("{e}\n{body}"))
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn metric(d: &Declared, n: &str) -> MetricJudge {
        match &d.kinds[n] {
            JudgeKind::Metric(m) => (**m).clone(),
            k => panic!("{k:?}"),
        }
    }

    /// A ratchet may leave its direction to its judge: `max:` ⇒ down, `min:` ⇒ up, a score ⇒ up.
    #[test]
    fn a_ratchet_takes_its_direction_from_a_metric_or_score_judge() {
        let c = cfg(
            "  cc: { metric: { id: complexity.cyclomatic, stat: p95 }, max: 10 }\n  cov: { metric: { id: coverage.line_percent }, min: 80 }\n  q: { score: { definition: swe-default, version: \"2\" }, min: 80 }\n",
            "    cc: { tolerance: 0.5 }\n    cov: {}\n    q: {}\n",
        );
        let d = declare(&c, &names(&["cc", "cov", "q"]), Path::new("/x"), false);
        assert!(d.problems.is_empty(), "{:?}", d.problems);
        let dirs: Vec<_> = d.ratchets.iter().map(|(n, r)| (n.as_str(), r.direction)).collect();
        assert_eq!(dirs, [("cc", Direction::Down), ("cov", Direction::Up), ("q", Direction::Up)]);
        assert_eq!(metric(&d, "cc").ratchet, Some(Direction::Down), "the judge checks it against the report");
        assert_eq!(d.bases["q"], "swe-default@2");
    }

    /// The refusals: a plain judge without a direction, a metric with both bounds, a score held
    /// down, a ratcheted `scope: changed` judge.
    #[test]
    fn a_ratchet_without_a_direction_anyone_states_is_refused() {
        let c = cfg(
            "  both: { metric: { id: a.b }, max: 10, min: 1 }\n  q: { score: { definition: swe-default, version: \"2\" }, min: 80 }\n  new: { metric: { id: a.b, scope: changed }, max: 1 }\n",
            "    plain: {}\n    both: {}\n    q: { direction: down }\n    new: { direction: down }\n",
        );
        let d = declare(&c, &names(&["plain", "both", "q", "new"]), Path::new("/x"), false);
        let at: Vec<&str> = d.problems.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(at, ["sequence.ratchet.both.direction", "sequence.ratchet.new", "sequence.ratchet.plain.direction", "sequence.ratchet.q.direction"]);
        assert!(d.ratchets.is_empty());
    }

    /// One scope, one measurement: its judges share the union of their families; a score in the run
    /// measures every family; a judge outside the run-set shapes nothing.
    #[test]
    fn judges_of_one_scope_share_one_measurement() {
        let j = "  a: { metric: { id: complexity.cyclomatic, stat: p95 }, max: 10 }\n  b: { metric: { id: duplication.percent }, max: 3 }\n  c: { metric: { id: secrets.findings, scope: changed }, max: 0 }\n  off: { metric: { id: docs.broken_links }, max: 0 }\n";
        let d = declare(&cfg(j, ""), &names(&["a", "b", "c"]), Path::new("/x"), false);
        let fams = ["complexity".to_string(), "duplication".to_string()];
        assert_eq!(metric(&d, "a").only.as_deref(), Some(&fams[..]));
        assert_eq!(metric(&d, "b").only, metric(&d, "a").only, "the same set ⇒ the same measurement");
        assert_eq!(metric(&d, "c").only.as_deref(), Some(&["secrets".to_string()][..]));
        let scored = format!("{j}  q: {{ score: {{ definition: swe-default, version: \"2\" }}, min: 80 }}\n");
        let d = declare(&cfg(&scored, ""), &names(&["a", "b", "c", "q"]), Path::new("/x"), false);
        assert_eq!(metric(&d, "a").only, None, "the score needs every family of the project scope");
        assert!(metric(&d, "c").only.is_some(), "…not of another scope");
        let sub = format!("{j}  w: {{ metric: {{ id: duplication.percent, scope: work }}, max: 3 }}\n  \
                           q: {{ score: {{ definition: swe-default, version: \"2\", scope: work }}, min: 80 }}\n");
        let d = declare(&cfg(&sub, ""), &names(&["a", "w", "q"]), Path::new("/x"), false);
        assert_eq!(metric(&d, "w").only, None, "a score of a sub-path needs every family of that sub-path");
        assert_eq!(metric(&d, "a").only.as_deref(), Some(&["complexity".to_string()][..]), "…not of the project");
    }

    /// A name declared here and as a file is two definitions of one judge.
    #[test]
    fn a_declared_judge_that_is_also_a_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("judges")).unwrap();
        std::fs::write(tmp.path().join("judges/cc.sh"), "#!/bin/sh\n").unwrap();
        let d = declare(&cfg("  cc: { metric: { id: a.b }, max: 1 }\n", ""), &names(&["cc"]), tmp.path(), false);
        assert_eq!(d.problems.len(), 1);
        assert!(d.problems[0].message.contains("one judge, one definition"), "{:?}", d.problems);
    }
}
