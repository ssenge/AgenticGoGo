//! The interpreter (A-37): reads a process file, refuses to run on any problem of `--check`, sets up agg and the
//! process's flow, and walks the phases and steps in file order, each kind through the block agg already has. The file
//! says what runs and in what order; nothing here names a step.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::anyhow;

use super::exec::{Ctx, Item};
use super::format::{Kind, Process, Section, Step};
use super::number::{number, resolve, Numbered};
use super::proc::{Init, Proc};
use super::settings::{Base, Settings};
use super::{bases, check, template};
use crate::driver::{Agg, Fatal, OnRegression, Opts};
use crate::plugin::RunOutcome;
use crate::workflow::{Roles, Run};

/// Runs the process in `file` on `project` as `args` say (`--from`, `--note`, …); starts again where the task force
/// sent it.
pub fn start(file: PathBuf, project: PathBuf, args: Vec<String>) -> ExitCode {
    crate::workflow::run(move || drive(&file, &project, &args))
}

/// Notes which process file runs on the project (`agg/private/process.json`), so `agg serve` and `agg status` can report it.
fn remember(project: &Path, file: &Path) {
    let dir = crate::paths::private_dir(project);
    let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join("process.json"), serde_json::json!({"file": file}).to_string());
    }
}

/// One run of the process, from its file to its end.
fn drive(file: &Path, project: &Path, args: &[String]) -> Result<(), Fatal> {
    let problems = check::check(file);
    if !problems.is_empty() {
        return Err(anyhow!("{} has {} problem(s):\n{}", file.display(), problems.len(), problems.join("\n")).into());
    }
    let home = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let spec = Process::parse(&std::fs::read_to_string(file).map_err(|e| anyhow!("{}: {e}", file.display()))?)?;
    let list = number(&spec);
    let project = std::fs::canonicalize(project).map_err(|e| anyhow!("{}: {e}", project.display()))?;
    let vars = template::variables(&spec, &list, &home, &project.display().to_string()).map_err(|e| anyhow!("{e}"))?;
    remember(&project, file);
    let mut run = Run::from_args(args)?;
    run.from = run.from.map(|f| resolve(&list, &f)).transpose().map_err(|e| anyhow!("--from {e}"))?;
    run.until = run.until.map(|f| resolve(&list, &f)).transpose().map_err(|e| anyhow!("--until {e}"))?;
    commit_inputs(&spec, &project)?;
    starting_commands(&spec, &home, &project)?;
    commit_inputs(&spec, &project)?;   // a start command may have written files
    if let (Some(step), true) = (&run.from, run.reset) {
        crate::workflow::take_back(&project, step, &spec.run.own)?;
        bases::drop_stale(&project, false);
        let _ = crate::workflow::commit(&project, &["."], "After the reset");
    }
    forget_stale_bases(&spec, &project);
    let settings = settings_of(&spec, &list, &home, &project, &vars)?;
    let roles = roles_of(&spec, &project)?;
    let mut agg = Agg::open_with(&project, Opts { resume: resumes(args) })?.memory(spec.agg.memory)
        .vars(vars.clone()).user_settings(spec.agg.user_settings)
        .on_regression(if spec.agg.on_regression == "rollback" { OnRegression::Rollback } else { OnRegression::Annotate })
        .judge_timeout(spec.agg.judge_timeout);
    if let Some(f) = &spec.run.system_prompt {
        agg = agg.system_prompt(std::fs::read_to_string(home.join(f)).map_err(|e| anyhow!("{f}: {e}"))?);
    }
    let force = task_force(&spec, &home, &vars, roles.as_ref())?;
    let pinned = pinned(&home);
    let p = Proc::new(agg, run, Init { spec: spec.clone(), list: list.clone(), home, settings, vars, roles, force });
    p.record("start", "start", true, &pinned);
    println!("{pinned}");
    for phase in &spec.phases {
        for step in &phase.steps {
            exec(&p, step, None)?;
        }
    }
    p.came_to_from()?;
    p.finish()?;
    p.finish_with(RunOutcome::GoalsMet, "the process is done");
    Ok(())
}

/// Whether the run replays agg's ledger: `--resume`, unless it starts at a step (`--from`, also a re-entry's
/// restart, which keeps the arguments it was started with), where the ledger no longer matches.
fn resumes(args: &[String]) -> bool {
    args.iter().any(|a| a == "--resume") && !args.iter().any(|a| a == "--from")
}

/// The commands `run.start` names, before agg opens the project; the last line each prints is logged.
fn starting_commands(spec: &Process, home: &Path, project: &Path) -> Result<(), Fatal> {
    for c in &spec.run.start {
        let first = c.split_whitespace().next().unwrap_or_default();
        let command = if first.contains('/') && !Path::new(first).is_absolute() { format!("{}/{c}", home.display()) } else { c.clone() };
        let out = std::process::Command::new("sh").arg("-c").arg(&command).current_dir(project)
            .env("AGG_PROCESS_HOME", home).output().map_err(|e| anyhow!("`{c}`: {e}"))?;
        if let Some(line) = String::from_utf8_lossy(&out.stdout).lines().last() {
            println!("{line}");
        }
        if !out.status.success() {
            let said = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!("`{c}` failed ({}): {}", out.status.code().unwrap_or(-1), said.trim().lines().last().unwrap_or_default()).into());
        }
    }
    Ok(())
}

/// Switches the project to the branch `run.commit_inputs` names and commits everything its folder holds, unless nothing
/// changed: the inputs a human filled in become part of the history, and a halted run's session branch is left.
fn commit_inputs(spec: &Process, project: &Path) -> Result<(), Fatal> {
    let Some(c) = &spec.run.commit_inputs else { return Ok(()) };
    let git = |args: &[&str]| std::process::Command::new("git").args(["-C", &project.display().to_string()]).args(args).output();
    let ok = |args: &[&str]| git(args).map(|o| o.status.success()).unwrap_or(false);
    if !ok(&["checkout", "-q", &c.branch]) {
        return Err(anyhow!("switching {} to {} failed", project.display(), c.branch).into());
    }
    ok(&["add", "-A"]);
    if !ok(&["diff", "--cached", "--quiet"]) && !ok(&["commit", "-q", "-m", &c.message]) {
        return Err(anyhow!("committing the inputs in {} failed", project.display()).into());
    }
    Ok(())
}

/// Which process this run uses: the commit of its home, and whether the checkout has uncommitted changes.
fn pinned(home: &Path) -> String {
    let git = |args: &[&str]| std::process::Command::new("git").args(["-C", &home.display().to_string()]).args(args).output();
    let Some(head) = git(&["rev-parse", "HEAD"]).ok().filter(|o| o.status.success()) else { return "process home not in git".into() };
    let dirty = git(&["status", "--porcelain"]).map(|o| !o.stdout.is_empty()).unwrap_or(false);
    format!("process commit {}{}", String::from_utf8_lossy(&head.stdout).trim(), if dirty { " with uncommitted changes" } else { "" })
}

/// Drops every step's base when the file `run.bases_reset_when_changed` differs from what the last run saw: the bases
/// describe the work of the content before it (a new increment).
fn forget_stale_bases(spec: &Process, project: &Path) {
    let Some(file) = &spec.run.bases_reset_when_changed else { return };
    let now = crate::protect::sha256::hex(&std::fs::read(project.join(file)).unwrap_or_default());
    let marker = project.join(".git").join("agg-bases-marker");
    if std::fs::read_to_string(&marker).ok().as_deref() != Some(now.as_str()) {
        bases::drop_stale(project, true);
        let _ = std::fs::write(marker, now);
    }
}

/// The settings: the process's own over the project's file, its names held to the process's steps.
pub(super) fn settings_of(spec: &Process, list: &[Numbered], home: &Path, project: &Path, vars: &BTreeMap<String, String>)
               -> Result<Settings, Fatal> {
    let mut steps = HashMap::new();
    fn walk(steps: &[Step], out: &mut HashMap<String, super::settings::StepSettings>) {
        for s in steps {
            if let Some(settings) = &s.settings {
                out.insert(s.id.clone(), settings.clone());
            }
            walk(&s.steps, out);
        }
    }
    for p in &spec.phases {
        walk(&p.steps, &mut steps);
    }
    let _ = home;
    let s = &spec.settings;
    let base = Base { defaults: s.defaults.clone(), steps, skip: s.skip.clone(),
        quality: super::settings::Quality { definition: s.quality.definition.clone(), version: s.quality.version.clone(), min: s.quality.min },
        models: s.models.clone() };
    let file = s.project.as_ref().map(|f| project.join(f));
    let settings = Settings::new(base, file.as_deref(), &s.extra_keys, vars)?;
    let ids: Vec<&str> = list.iter().map(|n| n.id.as_str()).chain(spec.phases.iter().map(|p| p.id.as_str())).collect();
    let cps: Vec<String> = list.iter().filter_map(|n| n.cp.clone()).collect();
    for id in settings.project_step_ids() {
        if !ids.contains(&id.as_str()) {
            return Err(anyhow!("the project's settings: `steps.{id}` names no step of the process").into());
        }
    }
    for (key, name) in settings.project_names() {
        let known = ids.contains(&name.as_str()) || cps.contains(&name) || (key == "auto_approve" && ["task-force", "all"].contains(&name.as_str()));
        if !known {
            return Err(anyhow!("the project's settings: `{key}: {name}` names no step, phase or checkpoint of the process").into());
        }
    }
    Ok(settings)
}

/// The roles file of the project: the worker's and the critic's agent, model and effort; a critic on the worker's
/// agent is refused (a review must be a second opinion).
fn roles_of(spec: &Process, project: &Path) -> Result<Option<Roles>, Fatal> {
    let Some(r) = &spec.roles else { return Ok(None) };
    let names: Vec<&str> = r.names.iter().map(String::as_str).collect();
    let roles = Roles::load(&project.join(&r.file), &names)?;
    if names.contains(&"worker") && names.contains(&"critic") {
        let agent = |n: &str| roles.get(n).agent.name().to_string();
        if agent("worker") == agent("critic") {
            return Err(anyhow!("{}: the critic must use another agent than the worker ({})", r.file, agent("worker")).into());
        }
    }
    Ok(Some(roles))
}

/// The task force's step: its prompt, what it may never change, the sections of its brief.
fn task_force(spec: &Process, home: &Path, vars: &BTreeMap<String, String>, roles: Option<&Roles>) -> Result<Option<crate::driver::Step>, Fatal> {
    let (Some(tf), Some(roles)) = (&spec.task_force, roles) else { return Ok(None) };
    let mut deny = vec![];
    for e in &tf.deny {
        match spec.deny.get(e) {
            Some(list) => deny.extend(list.iter().map(|g| template::render(g, vars)).collect::<Result<Vec<_>, _>>().map_err(|e| anyhow!("{e}"))?),
            None => deny.push(template::render(e, vars).map_err(|e| anyhow!("{e}"))?),
        }
    }
    let mut step = roles.step("worker", "task-force", home.join(&tf.prompt).display().to_string()).write_deny(deny);
    for s in &tf.sections {
        step = crate::workflow::with_section(&step, &plain_section(s, home, vars)?)?;
    }
    Ok(Some(step))
}

/// A section outside any step: its text or file rendered with the process's variables.
fn plain_section(s: &Section, home: &Path, vars: &BTreeMap<String, String>) -> Result<String, Fatal> {
    let text = match (&s.text, &s.file) {
        (Some(t), _) => t.clone(),
        (_, Some(f)) => std::fs::read_to_string(home.join(f)).map_err(|e| anyhow!("{f}: {e}"))?,
        _ => String::new(),
    };
    Ok(template::render(&text, vars).map_err(|e| anyhow!("{e}"))?)
}

/// Runs one step of the file (for an item of its `each`, if any), by its kind.
fn exec(p: &Proc, step: &Step, item: Option<Item>) -> Result<(), Fatal> {
    let Some(num) = p.list.iter().find(|n| n.id == step.id) else { return Ok(()) };
    match step.kind {
        Kind::Outside | Kind::Planned => return Ok(()),
        Kind::Each => return each(p, step),
        _ => {}
    }
    let ctx = p.ctx(step, num, item);
    p.register_constraints(&ctx)?;
    if p.off(&ctx)? || !p.applies_to(&ctx)? {
        return Ok(());
    }
    let reached = p.reaches(&ctx);
    if reached {
        p.commands(&ctx, &step.pre)?;
    }
    let done = kind(p, &ctx);
    if let (Err(_), Some(command)) = (&done, &step.on_fail) {
        // the step ends the run unmet: what it left up (a deployment) is taken down before the error goes on
        let said = p.run(&ctx, command).map_or_else(|e| e.to_string(), |o| o.lines().last().unwrap_or_default().to_string());
        eprintln!("{}: on_fail: {said}", ctx.label);
    }
    done?;
    if reached {
        p.commands(&ctx, &step.post)?;
    }
    Ok(())
}

/// The work of the step's kind.
fn kind(p: &Proc, ctx: &Ctx) -> Result<(), Fatal> {
    match ctx.step.kind {
        Kind::Script => p.script(ctx, ctx.step.run.as_deref().unwrap_or_default()),
        Kind::Agent => p.agent(ctx),
        Kind::Checkpoint => p.checkpoint(ctx),
        Kind::Exam => p.exam(ctx),
        Kind::Each | Kind::Outside | Kind::Planned => Ok(()),
    }
}

/// An `each`: its steps once per item, in order.
fn each(p: &Proc, step: &Step) -> Result<(), Fatal> {
    // a run that starts after the loop (`--from`) may have no items file yet (a project that never got that far)
    let before_from = p.run.from.as_deref().is_some_and(|f| !p.seen().iter().any(|s| s == f));
    if before_from && !items_file(p, step).is_some_and(|f| p.project.join(f).is_file()) {
        return Ok(());
    }
    for item in items(p, step)? {
        for child in &step.steps {
            exec(p, child, Some(item.clone()))?;
        }
    }
    Ok(())
}

/// The file an `each` takes its items from, its names filled in.
fn items_file(p: &Proc, step: &Step) -> Option<String> {
    template::render(&step.items.as_ref()?.file, &p.shared.vars).ok()
}

/// The items of an `each`: in the order its `list` gives, else in file order.
fn items(p: &Proc, step: &Step) -> Result<Vec<Item>, Fatal> {
    let spec = step.items.as_ref().ok_or_else(|| anyhow!("step `{}`: an `each` needs `items`", step.id))?;
    let file = template::render(&spec.file, &p.shared.vars).map_err(|e| anyhow!("step `{}`: {e}", step.id))?;
    let text = std::fs::read_to_string(p.project.join(&file)).map_err(|e| anyhow!("step `{}`: {file}: {e}", step.id))?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| anyhow!("step `{}`: {file}: {e}", step.id))?;
    let (list_key, items_key, id_key) = (spec.list.as_deref(), spec.items.as_deref().unwrap_or("units"), spec.key.as_deref().unwrap_or("id"));
    let all: Vec<&serde_yaml::Value> = doc.get(items_key).and_then(|v| v.as_sequence()).into_iter().flatten().collect();
    let id_of = |v: &serde_yaml::Value| v.get(id_key).and_then(|i| i.as_str()).map(String::from);
    let order: Vec<String> = match list_key {
        Some(k) => doc.get(k).and_then(|v| v.as_sequence()).into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect(),
        None => all.iter().filter_map(|v| id_of(v)).collect(),
    };
    let mut out = vec![];
    for (i, key) in order.iter().enumerate() {
        let value = all.iter().find(|v| id_of(v).as_deref() == Some(key.as_str()))
            .ok_or_else(|| anyhow!("step `{}`: {file}: no item `{key}`", step.id))?;
        out.push(Item { key: key.clone(), vars: item_vars(key, value), last: i + 1 == order.len() });
    }
    Ok(out)
}

/// An item's variables: `item`, `item.yaml`, and `item.<field>` for each field (a list joins with `, `).
fn item_vars(key: &str, value: &serde_yaml::Value) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::from([("item".to_string(), key.to_string()),
                                   ("item.yaml".to_string(), serde_yaml::to_string(value).unwrap_or_default())]);
    if let Some(map) = value.as_mapping() {
        for (k, v) in map {
            let Some(k) = k.as_str() else { continue };
            let text = match v {
                serde_yaml::Value::String(s) => s.clone(),
                serde_yaml::Value::Sequence(seq) => seq.iter().map(|x| x.as_str().map(String::from)
                    .unwrap_or_else(|| serde_yaml::to_string(x).unwrap_or_default().trim().to_string())).collect::<Vec<_>>().join(", "),
                other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
            };
            vars.insert(format!("item.{k}"), text);
        }
    }
    vars
}

impl Proc {
    /// What the task force's brief says the failed step keeps to: the step's own `constraints` and its review's rubric.
    pub fn register_constraints(&self, ctx: &Ctx) -> Result<(), Fatal> {
        let mut parts = vec![];
        if let Some(c) = &ctx.step.constraints {
            parts.push(format!("- {}", self.render(ctx, c)?));
        }
        if let Some(path) = self.rubric(ctx) {
            parts.push(format!("- **The step's review** (`{}`): its result is reviewed by this rubric. What it rules out a repair \
                doesn't add.", path.display()));
        }
        if !parts.is_empty() {
            self.shared.constraints.borrow_mut().insert(ctx.key.clone(), parts.join("\n\n"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resumed run that re-enters at a step starts there instead of replaying the ledger.
    #[test]
    fn a_run_from_a_step_does_not_replay_the_ledger() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(resumes(&args(&["p.yaml", "proj", "--resume"])));
        assert!(!resumes(&args(&["p.yaml", "proj", "--resume", "--from", "build", "--note", "x"])));
        assert!(!resumes(&args(&["p.yaml", "proj"])));
    }

    /// An item's variables: its key, its YAML, and each field, a list joined.
    #[test]
    fn an_items_variables_come_from_its_fields() {
        let v: serde_yaml::Value = serde_yaml::from_str("id: U1\nscope: [a/**, b/**]\nowner: me\nsize: 3\n").unwrap();
        let vars = item_vars("U1", &v);
        assert_eq!((vars["item"].as_str(), vars["item.scope"].as_str(), vars["item.owner"].as_str(), vars["item.size"].as_str()),
                   ("U1", "a/**, b/**", "me", "3"));
        assert!(vars["item.yaml"].contains("scope:"));
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").args(["-C", &dir.display().to_string()])
            .args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn spec(extra: &str) -> Process {
        Process::parse(&format!("version: 1\nrun:\n  commit_inputs: {extra}\nphases: []\n")).unwrap()
    }

    /// The inputs a human filled in are committed on the branch the file names; a second call with nothing new
    /// commits nothing; a branch that doesn't exist halts the run with the reason.
    #[test]
    fn the_inputs_are_committed_on_their_branch_once() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        git(d, &["init", "-q", "-b", "main"]);
        git(d, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(d, &["checkout", "-q", "-b", "agg/run/session-1"]);
        std::fs::write(d.join("task.md"), "the task").unwrap();
        let both = spec("{ branch: main, message: \"Inputs at start\" }");
        commit_inputs(&both, d).unwrap();
        assert_eq!(git(d, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "a halted run's session branch is left");
        assert_eq!(git(d, &["log", "-1", "--format=%s"]), "Inputs at start");
        assert_eq!(git(d, &["show", "--name-only", "--format=", "HEAD"]), "task.md");
        let before = git(d, &["rev-parse", "HEAD"]);
        commit_inputs(&both, d).unwrap();
        assert_eq!(git(d, &["rev-parse", "HEAD"]), before, "nothing new, no commit");
        std::fs::write(d.join("notes.md"), "more").unwrap();
        commit_inputs(&spec("{}"), d).unwrap();
        assert_eq!(git(d, &["log", "-1", "--format=%s"]), "Inputs at run start", "the defaults: main and this message");
        let err = commit_inputs(&spec("{ branch: nowhere }"), d).unwrap_err();
        assert!(format!("{err:?}").contains("to nowhere failed"), "{err:?}");
        assert!(commit_inputs(&Process::parse("version: 1\nphases: []\n").unwrap(), d).is_ok(), "without the key: nothing");
    }

    /// The process commit a run uses, with a note when its checkout has uncommitted changes; none in git says so.
    #[test]
    fn the_process_a_run_uses_is_pinned_by_its_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        assert_eq!(pinned(d), "process home not in git");
        git(d, &["init", "-q", "-b", "main"]);
        git(d, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let head = git(d, &["rev-parse", "HEAD"]);
        assert_eq!(pinned(d), format!("process commit {head}"));
        std::fs::write(d.join("x"), "y").unwrap();
        assert_eq!(pinned(d), format!("process commit {head} with uncommitted changes"));
    }
}
