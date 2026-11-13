//! The judges a process file declares under `judges:` (A-38): each made into a judge, and the problems of its
//! shape. A step's `checks` name them as `{ judge: <name> }`, the hooks files too.

use std::collections::BTreeMap;
use std::path::Path;

use super::format::Process;
use super::proc::Shared;
use super::{form, template};
use crate::core::config::TemplateSel;
use crate::core::model::{CommandJudge, Judge};
use crate::metrics::spec::{self, Declared};

/// The marker a hooks entry `{ judge: <name> }` becomes in the command lists the hooks pass around.
pub const HOOK: &str = "@judge:";

/// The declared judge `name` of the process, or why there is none. The project is where a score judge's definition file
/// is looked for and a template judge's document; the process's folder and names fill in a template judge's paths.
pub fn judge(shared: &Shared, name: &str) -> Result<Judge, String> {
    judge_of(&shared.spec, (&shared.project, &shared.home), &shared.vars, name)
}

/// [`judge`] from its parts: the process, the project and the process's folder, and the names for a template's paths.
pub fn judge_of(process: &Process, (project, home): (&Path, &Path), vars: &BTreeMap<String, String>, name: &str) -> Result<Judge, String> {
    let o = process.judges.get(name).ok_or_else(|| format!("no judge `{name}` is declared under `judges:`"))?;
    let judge = match (&o.metric, &o.score, &o.command, &o.template) {
        (_, _, Some(c), _) => Judge::command(name, CommandJudge::new(c.as_str()).path(o.path.iter().cloned())),
        (Some(m), _, _, _) => Judge::metric(name, spec::metric_judge(m, o.max, o.min)),
        (_, Some(s), _, _) => Judge::score(name, spec::score_judge(s, o.max, o.min, project)),
        (_, _, _, Some(t)) => template(home, vars, name, t)?,
        _ => return Err(format!("judge `{name}` declares none of `metric`, `score`, `command` or `template`")),
    };
    Ok(match o.timeout {
        Some(secs) => judge.timeout(secs),
        None => judge,
    })
}

/// A template judge: the document against the template, as [`super::form`] says.
fn template(home: &Path, vars: &BTreeMap<String, String>, name: &str, t: &TemplateSel) -> Result<Judge, String> {
    let fill = |what: &str, text: &str| template::render(text, vars).map_err(|e| format!("judges.{name}.{what}: {e}"));
    let (doc, from) = (fill("doc", &t.doc)?, fill("from", &t.from)?);
    let placeholder = t.placeholder.clone().unwrap_or_else(|| form::PLACEHOLDER.to_string());
    let template_file = home.join(&from);
    Ok(Judge::native(name, move |ctx| form::verdict(ctx.dir(), &doc, &template_file, &placeholder)))
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
        let others = [o.metric.is_some(), o.score.is_some(), o.command.is_some()];
        match (&o.template, others.iter().any(|x| *x)) {
            (Some(_), true) => out.push(format!("judges.{name}: a `template:` judge is that and nothing else")),
            (Some(t), false) => out.extend(form::problems("", "", t.placeholder.as_deref().unwrap_or(form::PLACEHOLDER)).err().map(|e| format!("judges.{name}: {e}"))),
            (None, false) => out.push(format!("judges.{name}: declares none of `metric`, `score`, `command` or `template`")),
            (None, true) => {}
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
        let tests = judge_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "tests").unwrap();
        assert!(matches!(&tests.kind, JudgeKind::Command(c) if c.command == "pytest -q" && c.path == [".venv/bin"]));
        assert_eq!(tests.timeout, Some(90));
        assert!(matches!(judge_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "parse").unwrap().kind, JudgeKind::Metric(_)));
        assert!(matches!(judge_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "q").unwrap().kind, JudgeKind::Score(_)));
        assert!(judge_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "nope").unwrap_err().contains("no judge `nope`"));
    }

    /// A template judge is a native judge over its paths with the process's names filled in (what it says is tested in
    /// `form`); it is that and nothing else.
    #[test]
    fn a_template_judge_is_declared_with_its_paths() {
        let p = process("  form: { template: { doc: \"{{ out }}.md\", from: form.md } }\n");
        let vars = BTreeMap::from([("out".to_string(), "report".to_string())]);
        let j = judge_of(&p, (Path::new("/p"), Path::new("/h")), &vars, "form").unwrap();
        assert!(matches!(j.kind, JudgeKind::Native { .. }));
        assert!(judge_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "form").unwrap_err().contains("judges.form.doc"));
        let both = process("  x: { template: { doc: a, from: b }, command: true }\n");
        assert!(problems(&both, Path::new("/p"))[0].contains("is that and nothing else"));
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
