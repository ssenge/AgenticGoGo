//! The verifiers a process file declares under `verifiers:` (A-38): each made into a verifier, and the problems of its
//! shape. A step's `checks` name them as `{ verifier: <name> }`, the hooks files too.

use std::collections::BTreeMap;
use std::path::Path;

use super::format::Process;
use super::proc::Shared;
use super::{form, template};
use crate::core::config::TemplateSel;
use crate::core::model::{CommandVerifier, Verifier};
use crate::metrics::spec::{self, Declared};

/// The marker a hooks entry `{ verifier: <name> }` becomes in the command lists the hooks pass around.
pub const HOOK: &str = "@verifier:";

/// The declared verifier `name` of the process, or why there is none. The project is where a score verifier's definition file
/// is looked for and a template verifier's document; the process's folder and names fill in a template verifier's paths.
pub fn verifier(shared: &Shared, name: &str) -> Result<Verifier, String> {
    verifier_of(&shared.spec, (&shared.project, &shared.home), &shared.vars, name)
}

/// [`verifier`] from its parts: the process, the project and the process's folder, and the names for a template's paths.
pub fn verifier_of(process: &Process, (project, home): (&Path, &Path), vars: &BTreeMap<String, String>, name: &str) -> Result<Verifier, String> {
    let o = process.verifiers.get(name).ok_or_else(|| format!("no verifier `{name}` is declared under `verifiers:`"))?;
    let verifier = match (&o.metric, &o.score, &o.command, &o.template) {
        (_, _, Some(c), _) => Verifier::command(name, CommandVerifier::new(c.as_str()).path(o.path.iter().cloned())),
        (Some(m), _, _, _) => Verifier::metric(name, spec::metric_verifier(m, o.max, o.min)),
        (_, Some(s), _, _) => Verifier::score(name, spec::score_verifier(s, o.max, o.min, project)),
        (_, _, _, Some(t)) => template(home, vars, name, t)?,
        _ => return Err(format!("verifier `{name}` declares none of `metric`, `score`, `command` or `template`")),
    };
    Ok(match o.timeout {
        Some(secs) => verifier.timeout(secs),
        None => verifier,
    })
}

/// A template verifier: the document against the template, as [`super::form`] says.
fn template(home: &Path, vars: &BTreeMap<String, String>, name: &str, t: &TemplateSel) -> Result<Verifier, String> {
    let fill = |what: &str, text: &str| template::render(text, vars).map_err(|e| format!("verifiers.{name}.{what}: {e}"));
    let (doc, from) = (fill("doc", &t.doc)?, fill("from", &t.from)?);
    let placeholder = t.placeholder.clone().unwrap_or_else(|| form::PLACEHOLDER.to_string());
    let template_file = home.join(&from);
    Ok(Verifier::native(name, move |ctx| form::verdict(ctx.dir(), &doc, &template_file, &placeholder)))
}

/// Every problem with the shape of the process file's `verifiers:`, each at its own key.
pub fn problems(process: &Process, home: &Path) -> Vec<String> {
    let mut out = vec![];
    for (name, o) in &process.verifiers {
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
            (Some(_), true) => out.push(format!("verifiers.{name}: a `template:` verifier is that and nothing else")),
            (Some(t), false) => out.extend(form::problems("", "", t.placeholder.as_deref().unwrap_or(form::PLACEHOLDER)).err().map(|e| format!("verifiers.{name}: {e}"))),
            (None, false) => out.push(format!("verifiers.{name}: declares none of `metric`, `score`, `command` or `template`")),
            (None, true) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::VerifierKind;

    fn process(verifiers: &str) -> Process {
        Process::parse(&format!("version: 1\nverifiers:\n{verifiers}phases: []\n")).unwrap()
    }

    /// Each kind of declaration becomes its verifier, with its timeout.
    #[test]
    fn a_declared_verifier_is_made_of_its_declaration() {
        let p = process(
            "  tests: { command: pytest -q, path: [.venv/bin], timeout: 90 }\n  parse: { metric: { id: syntax.parse_errors }, max: 0 }\n  q: { score: { definition: swe-default, version: \"0.4\" }, min: 90 }\n",
        );
        let tests = verifier_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "tests").unwrap();
        assert!(matches!(&tests.kind, VerifierKind::Command(c) if c.command == "pytest -q" && c.path == [".venv/bin"]));
        assert_eq!(tests.timeout, Some(90));
        assert!(matches!(verifier_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "parse").unwrap().kind, VerifierKind::Metric(_)));
        assert!(matches!(verifier_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "q").unwrap().kind, VerifierKind::Score(_)));
        assert!(verifier_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "nope").unwrap_err().contains("no verifier `nope`"));
    }

    /// A template verifier is a native verifier over its paths with the process's names filled in (what it says is tested in
    /// `form`); it is that and nothing else.
    #[test]
    fn a_template_verifier_is_declared_with_its_paths() {
        let p = process("  form: { template: { doc: \"{{ out }}.md\", from: form.md } }\n");
        let vars = BTreeMap::from([("out".to_string(), "report".to_string())]);
        let j = verifier_of(&p, (Path::new("/p"), Path::new("/h")), &vars, "form").unwrap();
        assert!(matches!(j.kind, VerifierKind::Native { .. }));
        assert!(verifier_of(&p, (Path::new("/p"), Path::new("/h")), &BTreeMap::new(), "form").unwrap_err().contains("verifiers.form.doc"));
        let both = process("  x: { template: { doc: a, from: b }, command: true }\n");
        assert!(problems(&both, Path::new("/p"))[0].contains("is that and nothing else"));
    }

    /// The shape problems of `rlt.yaml`'s declarations hold here too, and an empty declaration is one.
    #[test]
    fn a_declarations_shape_problems_are_listed_at_their_keys() {
        let p = process("  a: { command: x, max: 1 }\n  b: {}\n  c: { metric: { id: a.b } }\n");
        let found = problems(&p, Path::new("/p"));
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("verifiers.a: ") && found[1].starts_with("verifiers.b: ") && found[2].starts_with("verifiers.c: "));
        assert!(problems(&process("  ok: { command: true }\n"), Path::new("/p")).is_empty());
    }
}
