//! Hooks (A-37): lists of scripts that run before and after every step and every phase, the process's
//! (`run.hooks.process`, paths relative to the process home) and the project's (`run.hooks.project`, paths relative
//! to the project, which an agent may add to). A hook reads files and prints a verdict; its verdict goes to the run
//! record. Per step by its id (an `each`'s child: for every instance), per phase by its id.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::core::model::Judge;
use crate::workflow::Flow;

/// The scripts before and after one scope.
#[derive(Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct Lists {
    #[serde(default)]
    before: Vec<String>,
    #[serde(default)]
    after: Vec<String>,
}

/// A hooks file: for every step, per step by its id, per phase by its id.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct File {
    #[serde(default)]
    step: Lists,
    #[serde(default)]
    steps: HashMap<String, Lists>,
    #[serde(default)]
    phases: HashMap<String, Lists>,
}

/// Before or after.
#[derive(Clone, Copy, PartialEq)]
pub enum When {
    Before,
    After,
}

/// What a hook runs around: a step (its key and its id) or a phase (its id).
pub enum Scope<'a> {
    Step { key: &'a str, id: &'a str },
    Phase(&'a str),
}

/// A hooks file, or none when it doesn't exist or doesn't parse (a broken project file is reported by the
/// parse hook, never a reason to skip the process's).
pub fn read(path: &Path) -> File {
    std::fs::read_to_string(path).ok().and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default()
}

/// The hook commands of `scope` (`when`): the process's, then the project's, each with its script made absolute.
pub fn commands(process: (&File, &Path), project: (&File, &Path), scope: &Scope, when: When) -> Vec<String> {
    let pick = |l: &Lists| if when == When::Before { l.before.clone() } else { l.after.clone() };
    let of = |f: &File| -> Vec<String> {
        match scope {
            Scope::Step { key, id } => {
                let own = f.steps.get(*key).or_else(|| f.steps.get(*id)).map(pick).unwrap_or_default();
                [pick(&f.step), own].concat()
            }
            Scope::Phase(p) => f.phases.get(*p).map(pick).unwrap_or_default(),
        }
    };
    let absolute = |base: &Path, entry: &str| format!("{}/{}", base.display(), entry.trim());
    of(process.0).iter().map(|e| absolute(process.1, e)).chain(of(project.0).iter().map(|e| absolute(project.1, e))).collect()
}

/// A hook's short name: its script's file stem and arguments.
fn label(command: &str) -> String {
    let mut words = command.split_whitespace();
    let stem = words.next().map(|s| Path::new(s).file_stem().and_then(|f| f.to_str()).unwrap_or(s)).unwrap_or("hook");
    std::iter::once(stem).chain(words).collect::<Vec<_>>().join(" ")
}

/// The hooks of `scope` (`when`) as script judges, each told the step's key (`AGG_STEP`): what the run judges and
/// what a worker runs to check itself. A wrapper that can't be written is left out.
pub fn judges(flow: &Flow, commands: &[String], scope: &Scope, when: When) -> Vec<Judge> {
    let id = match scope {
        Scope::Step { key, .. } => (*key).to_string(),
        Scope::Phase(p) => format!("phase-{p}"),
    };
    let which = if when == When::Before { "before" } else { "after" };
    commands.iter().enumerate().filter_map(|(i, c)| {
        let path = flow.wrapper(&format!("hook-{id}-{which}-{i}.sh"), &format!("env AGG_STEP='{id}' {c}")).ok()?;
        Some(Judge::script(format!("hook-{}", label(c)).replace(' ', "-"), path.as_str()))
    }).collect()
}

/// Runs the hooks of a phase (`when`) and records each verdict under step `name`; the unmet ones' reasons.
pub fn check(flow: &Flow, name: &str, commands: &[String], scope: &Scope, when: When) -> Result<(), String> {
    let which = if when == When::Before { "before" } else { "after" };
    let which = match scope {
        Scope::Phase(p) => format!("phase {p} {which}"),
        Scope::Step { .. } => which.to_string(),
    };
    let failed: Vec<String> = judges(flow, commands, scope, when).iter().filter_map(|j| {
        let verdict = flow.judge(j);
        flow.record(name, "hook", verdict.met(), &format!("{which} {}: {}", j.name, verdict.rationale()));
        (!verdict.met()).then(|| format!("{}: {}", j.name, verdict.rationale()))
    }).collect();
    if failed.is_empty() { Ok(()) } else { Err(failed.join("; ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> File {
        serde_yaml::from_str(text).unwrap()
    }

    /// The process's hooks come first, then the project's; a step's own list adds to every step's; an instance
    /// of an `each`'s child takes its child's.
    #[test]
    fn a_steps_hooks_are_the_processs_then_the_projects() {
        let process = file("step: { after: [judges/parses.py] }\nsteps:\n  build: { after: [judges/own.py x] }\nphases:\n  verify: { before: [judges/secrets.py] }\n");
        let project = file("steps:\n  build: { after: [hooks/own.sh x] }\n");
        let (home, dir) = (Path::new("/home"), Path::new("/proj"));
        let after = commands((&process, home), (&project, dir), &Scope::Step { key: "build-U1", id: "build" }, When::After);
        assert_eq!(after, ["/home/judges/parses.py", "/home/judges/own.py x", "/proj/hooks/own.sh x"]);
        assert_eq!(label(&after[2]), "own x");
        let phase = commands((&process, home), (&project, dir), &Scope::Phase("verify"), When::Before);
        assert_eq!(phase, ["/home/judges/secrets.py"]);
        assert!(commands((&process, home), (&project, dir), &Scope::Phase("other"), When::After).is_empty());
    }
}
