//! A run of a process (A-37): agg's flow plus how this process's steps run: its settings, hooks
//! around steps and phases, the judge cache, the processes steps leave running, and the registries the flow's callbacks
//! need (a step's key, its phase). What the interpreter ([`super::run`]) and the step kinds work through.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::ops::Deref;
use std::path::PathBuf;
use std::rc::Rc;

use super::format::Process;
use super::hooks::{self, Scope, When};
use super::number::Numbered;
use super::settings::Settings;
use super::{bases, leftovers};
use crate::core::model::{Judge, JudgeKind};
use crate::driver::{Agg, Fatal, Step};
use crate::workflow::{Flow, Roles, Run};

/// What the flow's callbacks share with the run: they outlive the call that registered them, so they hold it by `Rc`.
pub struct Shared {
    pub spec: Process,
    pub home: PathBuf,
    pub project: PathBuf,
    pub vars: BTreeMap<String, String>,
    /// the step settings, for the hooks that read what a step declares
    pub settings: Settings,
    pub hooks: hooks::File,
    /// label → key, for the steps the callbacks are told by label
    pub keys: RefCell<HashMap<String, String>>,
    /// key → the step's id (an `each` instance → its child's id)
    pub ids: RefCell<HashMap<String, String>>,
    /// key → its phase's id
    pub phases: RefCell<HashMap<String, String>>,
    /// key → what the task force's brief says the failed step keeps to
    pub constraints: RefCell<HashMap<String, String>>,
    /// the phase the run is in
    pub phase: RefCell<Option<String>>,
}

/// What a run needs besides agg itself.
pub struct Init {
    pub spec: Process,
    pub list: Vec<Numbered>,
    pub home: PathBuf,
    pub settings: Settings,
    pub vars: BTreeMap<String, String>,
    pub roles: Option<Roles>,
    pub force: Option<Step>,
}

/// A run of a process: agg's flow, the process's settings and the registries.
pub struct Proc {
    flow: Flow,
    pub shared: Rc<Shared>,
    pub project: PathBuf,
    pub home: PathBuf,
    pub list: Vec<Numbered>,
    pub settings: Settings,
    pub roles: Option<Roles>,
}

impl Deref for Proc {
    type Target = Flow;
    fn deref(&self) -> &Flow {
        &self.flow
    }
}

impl Proc {
    /// The run on `agg`, starting and ending as `run` says, with the process's callbacks and its task force.
    pub fn new(agg: Agg, run: Run, init: Init) -> Proc {
        let project = agg.dir().to_path_buf();
        let spec = &init.spec;
        let _ = std::fs::remove_dir_all(project.join(".git").join("agg-verdicts"));
        if spec.run.leftovers {
            let ended = leftovers::watch(&project);
            if ended > 0 {
                agg.info(&format!("ended {ended} processes that earlier, stopped runs left running"));
            }
        }
        let hooks = spec.run.hooks.as_ref().and_then(|h| h.process.as_ref()).map(|f| hooks::read(&init.home.join(f))).unwrap_or_default();
        let shared = Rc::new(Shared { spec: init.spec.clone(), home: init.home.clone(), project: project.clone(),
            vars: init.vars, settings: init.settings.clone(), hooks, keys: RefCell::default(), ids: RefCell::default(), phases: RefCell::default(),
            constraints: RefCell::default(), phase: RefCell::default() });
        let mut flow = Flow::new(agg, run);
        if let Some(h) = &spec.run.history {
            flow = flow.history(h);
        }
        flow = flow.own(spec.run.own.clone());
        if let (Some(tf), Some(force)) = (&spec.task_force, init.force) {
            flow = flow.on_failure(force, tf.max);
        }
        let order: Vec<String> = init.list.iter().filter(|s| s.number.is_some()).map(|s| s.id.clone()).collect();
        flow = flow.order(order);
        let auto = init.settings.auto_approved("task-force", "", false);
        let (s1, s2, s3, s4) = (Rc::clone(&shared), Rc::clone(&shared), Rc::clone(&shared), Rc::clone(&shared));
        let flow = flow
            .constraints(move |f, name| s1.constraints.borrow().get(&f.key_of(name)).cloned())
            .trust(move |failed, at| auto || trusted(&s2, failed, at))
            .hooks(move |f, name, when| step_hooks(&s3, f, name, when))
            .on_start(move |f, name| starting(&s4, f, name));
        Proc { flow, shared, project, home: init.home, list: init.list, settings: init.settings, roles: init.roles }
    }

    /// The check run through the verdict cache (`agg cached`): the worker's self-check and the run's judge run it on
    /// the same content, and the second one is answered from the first.
    pub fn cached(&self, check: &Judge) -> Result<Judge, Fatal> {
        let JudgeKind::Script { path } = &check.kind else { return Ok(check.clone()) };
        let Some(cache) = &self.shared.spec.run.judge_cache else { return Ok(check.clone()) };
        let agg = std::env::current_exe().map_err(|e| anyhow::anyhow!("finding agg: {e}"))?;
        let ignore: String = cache.ignore.iter().map(|i| format!(" --ignore {}", super::exec::quote(i))).collect();
        let wrapper = self.wrapper(&format!("{}-cached.sh", check.name),
            &format!("{} cached{ignore} -- {}", super::exec::quote(&agg.display().to_string()), path.display()))?;
        Ok(Judge::script(check.name.clone(), wrapper.as_str()))
    }

    /// The run's end: what the last step left running ends, and the last phase's after-hooks and commands run.
    pub fn finish(&self) -> Result<(), Fatal> {
        reaped(&self.flow, &self.shared, "the run's end");
        let Some(phase) = self.shared.phase.borrow().clone() else { return self.flush() };
        let checked = leaving(&self.shared, &self.flow, "the run's end", &phase);
        self.flush()?;
        match checked {
            Ok(()) => Ok(()),
            Err(why) => self.fail("the run's end", &why),
        }
    }
}

/// Whether the task force's proposal goes ahead without a human: a rule of `task_force.trust` matches the failed step
/// (by its label) and the step to re-enter.
fn trusted(shared: &Shared, failed: &str, at: &str) -> bool {
    let key = shared.keys.borrow().get(failed).cloned().unwrap_or_default();
    let id = shared.ids.borrow().get(&key).cloned().unwrap_or_default();
    let phase = shared.phases.borrow().get(&key).cloned().unwrap_or_default();
    shared.spec.task_force.iter().flat_map(|tf| &tf.trust).any(|t| {
        t.reenter == at && (t.failed_in == phase || t.failed_in == id) && !t.except.contains(&id)
    })
}

/// The hooks around step `name`.
fn step_hooks(shared: &Shared, f: &Flow, name: &str, when: crate::workflow::When) -> Vec<Judge> {
    let when = if when == crate::workflow::When::Before { When::Before } else { When::After };
    let key = f.key_of(name);
    let id = shared.ids.borrow().get(&key).cloned().unwrap_or_else(|| key.clone());
    let scope = Scope::Step { key: &key, id: &id };
    let commands = hook_commands(shared, &scope, when);
    let mut judges = hooks::judges(f, &commands, &scope, when, &|n| declared_judge(shared, n));
    judges.extend(super::builtin::judges(shared, &key, &id, when));
    judges
}

/// The declared judge of the process file a hooks entry names.
fn declared_judge(shared: &Shared, name: &str) -> Option<Judge> {
    super::declared::judge(&shared.spec, &shared.project, name).ok()
}

/// The hook commands of `scope`: the process's file's, then the project's.
fn hook_commands(shared: &Shared, scope: &Scope, when: When) -> Vec<String> {
    let project = shared.spec.run.hooks.as_ref().and_then(|h| h.project.as_ref())
        .map(|p| hooks::read(&shared.project.join(p))).unwrap_or_default();
    hooks::commands((&shared.hooks, &shared.home), (&project, &shared.project), scope, when)
}

/// A step starts: what the step before it left running ends, its base is marked (the first start keeps it); when it
/// starts another phase, the last phase's after-hooks and commands and this one's before-hooks and commands run.
fn starting(shared: &Shared, flow: &Flow, name: &str) -> Result<(), String> {
    reaped(flow, shared, name);
    let key = flow.key_of(name);
    bases::mark(&shared.project, &key);
    let Some(now) = shared.phases.borrow().get(&key).cloned() else { return Ok(()) };
    let previous = shared.phase.replace(Some(now.clone()));
    if previous.as_deref() == Some(now.as_str()) {
        return Ok(());
    }
    if let Some(p) = &previous {
        leaving(shared, flow, name, p)?;
    }
    for c in phase_of(shared, &now).map(|p| p.pre.clone()).unwrap_or_default() {
        super::exec::run_plain(shared, &c, &now)?;
    }
    let commands = hook_commands(shared, &Scope::Phase(&now), When::Before);
    hooks::check(flow, name, &commands, &Scope::Phase(&now), When::Before, &|n| declared_judge(shared, n))
}

/// The phase `id` leaves: its after-hooks and its `post` commands.
fn leaving(shared: &Shared, flow: &Flow, name: &str, id: &str) -> Result<(), String> {
    let commands = hook_commands(shared, &Scope::Phase(id), When::After);
    hooks::check(flow, name, &commands, &Scope::Phase(id), When::After, &|n| declared_judge(shared, n))?;
    for c in phase_of(shared, id).map(|p| p.post.clone()).unwrap_or_default() {
        super::exec::run_plain(shared, &c, id)?;
    }
    Ok(())
}

fn phase_of<'a>(shared: &'a Shared, id: &str) -> Option<&'a super::format::Phase> {
    shared.spec.phases.iter().find(|p| p.id == id)
}

/// Ends what the step before `name` left running, and says so.
fn reaped(flow: &Flow, shared: &Shared, name: &str) {
    if !shared.spec.run.leftovers {
        return;
    }
    let ended = leftovers::reap();
    if ended > 0 {
        flow.info(&format!("ended {ended} processes the step before {name} left running"));
    }
}
