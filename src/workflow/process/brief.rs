//! `agg process <file> <project> --brief <step>` (A-38 3.8): the process's parts of what a step's worker and critic
//! get, without running anything: the system prompt, the worker's prompt, the standing sections, the step's own
//! sections, the quality guide and the critic's scope and rubric; `--brief task-force` is the task force's prompt. A
//! name the step only knows at run time (`{{ item }}`) and a section's command stay as written.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{anyhow, bail, Result};

use super::format::{Kind, Process, Review, Step};
use super::number::{number, resolve, Numbered};
use super::{standing, template};

/// `text` with the names `vars` has filled in; others stay as they are.
fn fill(text: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        out.push_str(&rest[..start]);
        match vars.get(after[..end].trim()) {
            Some(v) => out.push_str(v),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

fn find<'a>(steps: &'a [Step], id: &str) -> Option<&'a Step> {
    steps.iter().find_map(|s| if s.id == id { Some(s) } else { find(&s.steps, id) })
}

/// A file of the process's folder, filled in.
fn file(home: &Path, name: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    Ok(fill(&std::fs::read_to_string(home.join(name)).map_err(|e| anyhow!("{name}: {e}"))?, vars))
}

/// The brief of the agent step `step` as headed parts.
fn parts(spec: &Process, home: &Path, project: &Path, step: &Step, num: &Numbered, vars: &BTreeMap<String, String>) -> Result<Vec<(String, String)>> {
    let mut vars = vars.clone();
    vars.extend([("this.id".to_string(), step.id.clone()), ("this.key".into(), step.id.clone()), ("this.name".into(), step.name.clone()),
                 ("this.number".into(), num.number.clone().unwrap_or_default())]);
    let mut out = vec![("system prompt".to_string(), fill(&standing::text(spec.run.system_prompt.as_ref(), standing::SYSTEM, home)?, &vars))];
    let prompt = step.prompt.clone().unwrap_or_else(|| format!("{}/{}.md", spec.dirs.prompts, step.id));
    out.push((format!("the worker's prompt ({prompt})"), file(home, &prompt, &vars)?));
    for s in &spec.run.brief.sections {
        out.push(("a standing section".into(), section(home, s, &vars)?));
    }
    if spec.run.brief.shared_machine {
        out.push(("the shared machine".into(), standing::SHARED_MACHINE.trim_end().to_string()));
    }
    if let Some(notes) = &spec.run.notes {
        out.push((format!("the notes ({notes})"), fill(&standing::notes_section(notes), &vars)));
    }
    for s in &step.sections {
        out.push(("a section of the step".into(), section(home, s, &vars)?));
    }
    if let (true, Some(guide)) = (step.quality, &spec.settings.quality.guide) {
        out.push((format!("the quality guide ({guide})"), file(home, guide, &vars)?));
    }
    let own = spec.settings.project.as_ref().map(|f| project.join(f));
    if own.as_ref().is_some_and(|f| f.is_file()) {
        out.push(("the project's settings".into(), format!("{} (its note for the step, `inputs` and `outputs` are read from it)", own.unwrap_or_default().display())));
    }
    let review = match &step.review {
        Some(Review::File(f)) => Some(f.clone()),
        Some(Review::On(true)) => Some(format!("{}/{}-review.md", spec.dirs.reviews, step.id)),
        _ => None,
    };
    if let Some(rubric) = review {
        let scope = standing::text(spec.run.critic_scope.as_ref(), standing::CRITIC_SCOPE, home)?;
        out.push(("the critic's scope".into(), fill(&scope, &vars)));
        out.push((format!("the critic's rubric ({rubric})"), file(home, &rubric, &vars)?));
    }
    Ok(out)
}

/// A brief section: its text, its file, or a note of the command it runs.
fn section(home: &Path, s: &super::format::Section, vars: &BTreeMap<String, String>) -> Result<String> {
    match (&s.text, &s.file, &s.run) {
        (Some(t), _, _) => Ok(fill(t, vars)),
        (_, Some(f), _) => file(home, f, vars),
        (_, _, Some(c)) => Ok(format!("(the output of `{c}` at run time)")),
        _ => Ok(String::new()),
    }
}

/// Prints the brief of `target` (a step's id, number or instance key, or `task-force`).
pub fn command(file: &Path, project: &Path, target: &str) -> Result<ExitCode> {
    let spec = Process::parse(&std::fs::read_to_string(file).map_err(|e| anyhow!("{}: {e}", file.display()))?).map_err(|e| anyhow!("{}: {e}", file.display()))?;
    let list = number(&spec);
    let home = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let vars = template::variables(&spec, &list, &home, &project.display().to_string()).map_err(|e| anyhow!("{e}"))?;
    let shown: Vec<(String, String)> = if target == "task-force" {
        let Some(tf) = &spec.task_force else { bail!("the process has no task force") };
        vec![("the task force's prompt".into(), fill(&standing::text(tf.prompt.as_ref(), standing::TASK_FORCE, &home)?, &vars))]
    } else {
        let key = resolve(&list, target).map_err(|e| anyhow!("--brief {e}"))?;
        let num = list.iter().find(|n| key == n.id || key.starts_with(&format!("{}-", n.id))).ok_or_else(|| anyhow!("--brief: `{target}` names no step"))?;
        let step = spec.phases.iter().find_map(|p| find(&p.steps, &num.id)).ok_or_else(|| anyhow!("--brief: no step {}", num.id))?;
        if !matches!(step.kind, Kind::Agent) {
            bail!("--brief: `{}` is a {:?} step: only an agent step has a worker's brief", step.id, step.kind);
        }
        parts(&spec, &home, project, step, num, &vars)?
    };
    for (heading, text) in shown {
        println!("===== {heading} =====\n{}\n", text.trim_end());
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name the vars have is filled in; others (`{{ item }}`) stay.
    #[test]
    fn a_known_name_is_filled_and_an_unknown_one_stays() {
        let vars = BTreeMap::from([("a".to_string(), "1".to_string())]);
        assert_eq!(fill("x {{ a }} y {{ item }} z {{ a", &vars), "x 1 y {{ item }} z {{ a");
    }
}
