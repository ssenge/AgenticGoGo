//! The judges a process file declares under `judges:` (A-38): each made into a judge, and the problems of its
//! shape. A step's `checks` name them as `{ judge: <name> }`, the hooks files too.

use std::path::Path;

use super::format::Process;
use crate::core::model::{CommandJudge, Judge};
use crate::metrics::spec::{self, Declared};

/// The marker a hooks entry `{ judge: <name> }` becomes in the command lists the hooks pass around.
pub const HOOK: &str = "@judge:";

/// The declared judge `name` of the process, or why there is none. `project` is where a score judge's definition
/// file is looked for.
pub fn judge(process: &Process, project: &Path, name: &str) -> Result<Judge, String> {
    let o = process.judges.get(name).ok_or_else(|| format!("no judge `{name}` is declared under `judges:`"))?;
    let judge = match (&o.metric, &o.score, &o.command) {
        (_, _, Some(c)) => Judge::command(name, CommandJudge::new(c.as_str()).path(o.path.iter().cloned())),
        (Some(m), _, _) => Judge::metric(name, spec::metric_judge(m, o.max, o.min)),
        (_, Some(s), _) => Judge::score(name, spec::score_judge(s, o.max, o.min, project)),
        _ => return Err(format!("judge `{name}` declares none of `metric`, `score` or `command`")),
    };
    Ok(match o.timeout {
        Some(secs) => judge.timeout(secs),
        None => judge,
    })
}

/// Every problem with the shape of the process file's `judges:`, each at its own key.
pub fn problems(process: &Process, home: &Path) -> Vec<String> {
    let mut out = vec![];
    for (name, o) in &process.judges {
        let shape = Declared {
            name,
            metric: o.metric.as_ref(),
            score: o.score.as_ref(),
            max: o.max,
            min: o.min,
            command: o.command.as_deref(),
            path: &o.path,
        };
        out.extend(spec::problems(&shape, home).iter().map(ToString::to_string));
        if o.metric.is_none() && o.score.is_none() && o.command.is_none() {
            out.push(format!("judges.{name}: declares none of `metric`, `score` or `command`"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::JudgeKind;

    fn process(judges: &str) -> Process {
        Process::parse(&format!("version: 1\njudges:\n{judges}phases: []\n")).unwrap()
    }

    /// Each kind of declaration becomes its judge, with its timeout.
    #[test]
    fn a_declared_judge_is_made_of_its_declaration() {
        let p = process(
            "  tests: { command: pytest -q, path: [.venv/bin], timeout: 90 }\n  parse: { metric: { id: syntax.parse_errors }, max: 0 }\n  q: { score: { definition: swe-default, version: \"0.4\" }, min: 90 }\n",
        );
        let tests = judge(&p, Path::new("/p"), "tests").unwrap();
        assert!(matches!(&tests.kind, JudgeKind::Command(c) if c.command == "pytest -q" && c.path == [".venv/bin"]));
        assert_eq!(tests.timeout, Some(90));
        assert!(matches!(judge(&p, Path::new("/p"), "parse").unwrap().kind, JudgeKind::Metric(_)));
        assert!(matches!(judge(&p, Path::new("/p"), "q").unwrap().kind, JudgeKind::Score(_)));
        assert!(judge(&p, Path::new("/p"), "nope").unwrap_err().contains("no judge `nope`"));
    }

    /// The shape problems of `agg.yaml`'s declarations hold here too, and an empty declaration is one.
    #[test]
    fn a_declarations_shape_problems_are_listed_at_their_keys() {
        let p = process("  a: { command: x, max: 1 }\n  b: {}\n  c: { metric: { id: a.b } }\n");
        let found = problems(&p, Path::new("/p"));
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("judges.a: ") && found[1].starts_with("judges.b: ") && found[2].starts_with("judges.c: "));
        assert!(problems(&process("  ok: { command: true }\n"), Path::new("/p")).is_empty());
    }
}
