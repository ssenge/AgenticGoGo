//! `{{ name }}` in a process file's strings (A-37 §4.6): the file's `vars`, the built-ins (`home`, `project`,
//! `step.<id>`, `cp.<id>`), and inside an `each` the item's. Unlike a prompt's lenient rendering (a project's own
//! `{{ a.b }}` stays), a name the file's strings don't know is an error: a command with a hole is the wrong command.

use std::collections::BTreeMap;

use super::format::Process;
use super::number::Numbered;

/// `text` with every `{{ name }}` filled in from `vars`; a name that isn't there is an error naming it.
pub fn render(text: &str, vars: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}").map(|end| (end, after[..end].trim())) {
            Some((end, name)) if is_name(name) => {
                out.push_str(vars.get(name).ok_or_else(|| format!("unknown variable `{{{{ {name} }}}}`"))?);
                rest = &after[end + 2..];
            }
            _ => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Whether `s` is a name: parts of letters, digits, `_` and `-` joined by `.`, the first starting with a letter.
fn is_name(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
}

/// The variables of the process file: `vars_file` then `vars` (values may name `{{ home }}` and `{{ env.NAME }}`),
/// then `home`, `project`, one `step.<id>`, `step.<id>.number`, `step.<id>.name` per step, `cp.<id>` per checkpoint and
/// `phase.<id>`, `phase.<id>.number`, `phase.<id>.name` per phase.
pub fn variables(process: &Process, list: &[Numbered], home: &std::path::Path, project: &str) -> Result<BTreeMap<String, String>, String> {
    let mut vars = BTreeMap::new();
    if let Some(file) = &process.vars_file {
        let path = home.join(file);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("vars_file {}: {e}", path.display()))?;
        let map: BTreeMap<String, String> = serde_yaml::from_str(&text).map_err(|e| format!("vars_file {}: {e}", path.display()))?;
        vars.extend(map);
    }
    vars.extend(process.vars.clone());
    // absolute, so a path built on it (`{{ home }}/../templates`) means the same wherever it is joined or used from
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let mut base: BTreeMap<String, String> = BTreeMap::from([("home".into(), home.display().to_string()),
                                                             ("project".into(), project.to_string())]);
    for (name, value) in std::env::vars() {
        base.insert(format!("env.{name}"), value);
    }
    let mut out = BTreeMap::new();
    for (name, value) in vars {
        // an unset `env.NAME` is an empty string here: the file says what it needs by a check of its own
        let value = render_env(&value, &base).map_err(|e| format!("vars: {name}: {e}"))?;
        out.insert(name, value);
    }
    out.extend(base.into_iter().filter(|(n, _)| !n.starts_with("env.")));
    for (i, phase) in process.phases.iter().enumerate() {
        out.insert(format!("phase.{}", phase.id), format!("{} {}", i + 1, phase.name));
        out.insert(format!("phase.{}.number", phase.id), (i + 1).to_string());
        out.insert(format!("phase.{}.name", phase.id), phase.name.clone());
    }
    for s in list {
        let Some(label) = s.label() else { continue };
        let number = s.number.clone().unwrap_or_default();
        out.insert(format!("step.{}", s.id), label);
        out.insert(format!("step.{}.number", s.id), number);
        out.insert(format!("step.{}.name", s.id), s.name.clone());
        if let Some(cp) = &s.cp {
            out.insert(format!("cp.{}", s.id), cp.clone());
        }
    }
    Ok(out)
}

/// A `vars` value with `{{ home }}`, `{{ project }}` and `{{ env.NAME }}` filled in (an unset `env.NAME` is empty).
fn render_env(text: &str, base: &BTreeMap<String, String>) -> Result<String, String> {
    let mut with_env = base.clone();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find("}}") {
            let name = after[..end].trim();
            if name.starts_with("env.") {
                with_env.entry(name.to_string()).or_default();
            }
            rest = &after[end + 2..];
        } else {
            break;
        }
    }
    render(text, &with_env)
}

/// The names a prompt or rubric file uses that the process can't fill in: its plain names and its `step.` and
/// `cp.` names that aren't in `vars` (a project's own `{{ a.b }}` is none of the process's business).
pub fn unknown_in_file(text: &str, vars: &BTreeMap<String, String>) -> Vec<String> {
    let mut found = vec![];
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim();
        let plain = is_name(name) && !name.contains('.');
        let ours = ["step.", "cp.", "phase."].iter().any(|p| name.starts_with(p));
        if is_name(name) && (plain || ours) && !vars.contains_key(name) {
            found.push(name.to_string());
        }
        rest = &after[end + 2..];
    }
    found.sort();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> BTreeMap<String, String> {
        BTreeMap::from([("legacy".into(), "old".into()), ("step.build".into(), "2.1 Build".into())])
    }

    /// Names are filled in; an unknown one is an error; text that isn't a name stays.
    #[test]
    fn names_are_filled_in_and_unknown_ones_refused() {
        assert_eq!(render("{{ legacy }}/x, {{step.build}}", &vars()).unwrap(), "old/x, 2.1 Build");
        assert_eq!(render("{{ nowhere }}", &vars()).unwrap_err(), "unknown variable `{{ nowhere }}`");
        assert!(render("{{ step.typo }}", &vars()).is_err());
        assert_eq!(render("{{ 1x }} {{ a b }} {{ unclosed", &vars()).unwrap(), "{{ 1x }} {{ a b }} {{ unclosed");
    }

    /// A prompt's own template language isn't ours; its plain names and the process's `step.` and `cp.` names are.
    #[test]
    fn a_file_is_checked_for_the_names_the_process_owns() {
        let text = "{{ legacy }} {{ other }} {{ step.build }} {{ step.nope }} {{ cp.x }} ${{ github.sha }} {{ item.name }}";
        assert_eq!(unknown_in_file(text, &vars()), ["cp.x", "other", "step.nope"]);
    }

    /// `home`, `project`, an `env.NAME` in a value, and the step and checkpoint names come with the file's vars.
    #[test]
    fn the_variables_hold_the_builtins_and_the_numbered_steps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("layout.yaml"), "legacy: legacy\n").unwrap();
        let p = Process::parse("version: 1\nvars_file: layout.yaml\nvars: { tools: \"{{ home }}/tools\", mine: \"{{ env.AGG_NO_SUCH_VAR }}x\" }\n\
            phases:\n- id: p\n  name: P\n  steps:\n  - { id: s, name: S, kind: script, run: x }\n  - { id: c, name: C, kind: checkpoint, prepare: p, doc: d, question: q }\n").unwrap();
        let list = super::super::number::number(&p);
        let v = variables(&p, &list, dir.path(), "/proj").unwrap();
        assert_eq!(v["legacy"], "legacy");
        assert_eq!(v["tools"], format!("{}/tools", dir.path().canonicalize().unwrap().display()), "{{ home }} is absolute");
        assert_eq!(v["mine"], "x", "an unset env name is empty");
        assert_eq!((v["step.s"].as_str(), v["step.s.number"].as_str(), v["cp.c"].as_str(), v["project"].as_str()),
                   ("1.1 S", "1.1", "CP1", "/proj"));
        assert_eq!(v["step.c"], "1.2 CP1 C");
        assert_eq!((v["phase.p"].as_str(), v["phase.p.number"].as_str(), v["phase.p.name"].as_str()), ("1 P", "1", "P"));
    }
}
