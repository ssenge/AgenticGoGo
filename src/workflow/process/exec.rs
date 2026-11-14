//! Running the commands of a process file (A-37): a step's context (its key, label, item), the `{{ names }}` of its
//! strings, a command made absolute and wrapped with the environment every script gets, and a script judge from it.

use std::collections::BTreeMap;
use std::process::Command;

use super::format::Step;
use super::number::Numbered;
use super::proc::{Proc, Shared};
use super::template;
use crate::core::model::Judge;
use crate::driver::Fatal;

/// The item of an `each` a step instance is for.
#[derive(Clone)]
pub struct Item {
    pub key: String,
    /// `item`, `item.yaml`, and `item.<field>` for each field of the item
    pub vars: BTreeMap<String, String>,
    pub last: bool,
}

/// What a step execution knows: its definition, numbers, key and item.
pub struct Ctx<'a> {
    pub step: &'a Step,
    pub num: &'a Numbered,
    pub key: String,
    pub label: String,
    pub item: Option<Item>,
}

impl Ctx<'_> {
    pub fn id(&self) -> &str {
        &self.step.id
    }
}

/// `v` as one shell word.
pub fn quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

impl Proc {
    /// The context of `step` (for the item of its `each`, if any), its key and label registered where the flow's
    /// callbacks find them.
    pub fn ctx<'a>(&'a self, step: &'a Step, num: &'a Numbered, item: Option<Item>) -> Ctx<'a> {
        let (key, label) = match &item {
            Some(i) => (format!("{}-{}", step.id, i.key), num.instance_label(&i.key).unwrap_or_default()),
            None => (step.id.clone(), num.label().unwrap_or_default()),
        };
        self.register(&label, &key, &step.id, &num.phase_id);
        Ctx { step, num, key, label, item }
    }

    /// Registers step `label` under `key` (its id `id`, in phase `phase`) for the flow and its callbacks.
    pub fn register(&self, label: &str, key: &str, id: &str, phase: &str) {
        self.name_key(label, key);
        self.shared.keys.borrow_mut().insert(label.to_string(), key.to_string());
        self.shared.ids.borrow_mut().insert(key.to_string(), id.to_string());
        self.shared.phases.borrow_mut().insert(key.to_string(), phase.to_string());
    }

    /// The variables of a step's strings: the process's, `this.*`, and an item's with `before` and `base`.
    pub fn scope(&self, ctx: &Ctx) -> BTreeMap<String, String> {
        let mut vars = self.shared.vars.clone();
        vars.insert("this".into(), ctx.label.clone());
        vars.insert("this.id".into(), ctx.id().to_string());
        vars.insert("this.key".into(), ctx.key.clone());
        vars.insert("this.name".into(), ctx.num.name.clone());
        vars.insert("this.number".into(), ctx.num.number.clone().unwrap_or_default());
        if let Some(item) = &ctx.item {
            vars.extend(item.vars.clone());
            vars.insert("before".into(), crate::workflow::before(&ctx.key));
            vars.insert("base".into(), super::bases::base(&ctx.key));
            // the refs of the item's instances of the `each`'s other steps: `{{ before.<id> }}`
            for sibling in self.list.iter().filter(|n| n.parent.is_some() && n.parent == ctx.num.parent) {
                let key = format!("{}-{}", sibling.id, item.key);
                vars.insert(format!("before.{}", sibling.id), crate::workflow::before(&key));
                vars.insert(format!("base.{}", sibling.id), super::bases::base(&key));
            }
        }
        vars
    }

    /// `text` with its `{{ names }}` filled in for `ctx`.
    pub fn render(&self, ctx: &Ctx, text: &str) -> Result<String, Fatal> {
        template::render(text, &self.scope(ctx)).map_err(|e| anyhow::anyhow!("step `{}`: {e}", ctx.key).into())
    }

    /// A command rendered, its first word made absolute when it is a path relative to the process home.
    pub fn command(&self, ctx: &Ctx, text: &str) -> Result<String, Fatal> {
        Ok(absolutize(&self.shared, &self.render(ctx, text)?))
    }

    /// The environment every script of a step gets.
    fn env(&self, ctx: &Ctx) -> String {
        let mut env = vec![format!("AGG_STEP={}", quote(&ctx.key)), format!("AGG_STEP_LABEL={}", quote(&ctx.label)),
                           format!("AGG_PROCESS_HOME={}", quote(&self.home.display().to_string()))];
        if let Some(i) = &ctx.item {
            env.push(format!("AGG_ITEM={}", quote(&i.key)));
        }
        env.join(" ")
    }

    /// A script judge from `command`: a wrapper that runs it with the step's environment, named `name` or after the
    /// step and the command.
    pub fn judge(&self, ctx: &Ctx, name: Option<&str>, command: &str) -> Result<Judge, Fatal> {
        let command = self.command(ctx, command)?;
        let name = name.map(String::from).unwrap_or_else(|| format!("{}-{}", ctx.key, slug(&command)));
        let wrapper = self.wrapper(&format!("{name}.sh"), &format!("env {} {command}", self.env(ctx)))?;
        Ok(Judge::script(name, wrapper.as_str()))
    }

    /// Runs `command` for `ctx` to its end in the project; its stdout, or why it failed.
    pub fn run(&self, ctx: &Ctx, command: &str) -> Result<String, Fatal> {
        let command = self.command(ctx, command)?;
        let out = Command::new("sh").arg("-c").arg(format!("env {} {command}", self.env(ctx))).current_dir(&self.project)
            .output().map_err(|e| anyhow::anyhow!("running `{command}`: {e}"))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).to_string());
        }
        let said = String::from_utf8_lossy(&out.stderr);
        let said = said.trim().lines().last().unwrap_or_default();
        Err(anyhow::anyhow!("`{command}` failed ({}): {said}", out.status.code().unwrap_or(-1)).into())
    }
}

/// A command of the process file run for a phase (`pre`, `post`): made absolute, run in the project, its failure the
/// reason the step that starts the phase fails.
pub fn run_plain(shared: &Shared, command: &str, phase: &str) -> Result<(), String> {
    let mut vars = shared.vars.clone();
    vars.insert("this".into(), phase.to_string());
    let command = template::render(command, &vars).map_err(|e| format!("phase `{phase}`: {e}"))?;
    let command = absolutize(shared, &command);
    let out = Command::new("sh").arg("-c").arg(format!("env AGG_PHASE={} AGG_PROCESS_HOME={} {command}", quote(phase),
        quote(&shared.home.display().to_string()))).current_dir(&shared.project).output().map_err(|e| format!("`{command}`: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    Err(format!("`{command}` failed ({}): {}", out.status.code().unwrap_or(-1), said.trim().lines().last().unwrap_or_default()))
}

/// `command` with its first word made absolute when it is a relative path (it has a `/`).
fn absolutize(shared: &Shared, command: &str) -> String {
    absolutize_in(&shared.home, command)
}

/// A command of the process file with its script made absolute: a relative path is the process home's.
pub fn absolutize_in(home: &std::path::Path, command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or_default();
    if first.contains('/') && !std::path::Path::new(first).is_absolute() {
        return format!("{}/{}{}", home.display(), first, &command[command.find(first).unwrap_or(0) + first.len()..]);
    }
    command.to_string()
}

/// A command as a name: its script's stem and arguments, letters and digits only.
fn slug(command: &str) -> String {
    let mut words = command.split_whitespace();
    let stem = words.next().map(|s| std::path::Path::new(s).file_stem().and_then(|f| f.to_str()).unwrap_or(s)).unwrap_or("check");
    let all: String = std::iter::once(stem).chain(words).collect::<Vec<_>>().join("-");
    let mut out: String = all.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').chars().take(60).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        let spec = super::super::format::Process::parse("version: 1\nphases:\n- id: p\n  name: P\n  steps:\n  - { id: s, name: S, kind: outside }\n").unwrap();
        Shared { spec, home: "/home".into(), project: "/proj".into(), vars: BTreeMap::new(), settings: Default::default(), hooks: Default::default(),
                 keys: Default::default(), ids: Default::default(), phases: Default::default(), constraints: Default::default(),
                 phase: Default::default() }
    }

    /// A relative script path is made absolute against the process home, with its arguments kept; others stay.
    #[test]
    fn a_relative_script_is_made_absolute_and_a_plain_command_stays() {
        let s = shared();
        assert_eq!(absolutize(&s, "judges/has.py ui"), "/home/judges/has.py ui");
        assert_eq!(absolutize(&s, "/abs/x.sh a/b"), "/abs/x.sh a/b");
        assert_eq!(absolutize(&s, "sh -c 'echo a/b'"), "sh -c 'echo a/b'");
    }

    /// A command is named by its script's stem and arguments; a quote is one shell word.
    #[test]
    fn a_command_has_a_name_and_a_quote_is_one_word() {
        assert_eq!(slug("/home/judges/capture.py record --since refs/x"), "capture-record-since-refs-x");
        assert_eq!(quote("it's"), "'it'\\''s'");
    }
}
