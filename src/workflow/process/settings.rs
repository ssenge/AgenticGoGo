//! How each step of a process runs (A-37): its worker's agent, model and effort, its critic
//! (or none), its repair rounds, what it reads and writes, its tools, its ladder and its note. The process
//! file gives the defaults, a project's file overrides them: per step by its id (also for every instance of an
//! `each`'s child), or for every step under `defaults`. Execution settings only, never policy.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{anyhow, Context as _, Result};
use serde::Deserialize;

use super::ladder::TierSpec;
use crate::core::model::Judge;
use crate::driver::Step;

/// A role's settings; each one unset falls back to the layer below.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// A step's critic: `false` for none, `true` for the default one, or a role.
#[derive(Deserialize, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Critic {
    On(bool),
    Role(Role),
}

/// How one step runs; each setting unset falls back to the layer below.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSettings {
    pub worker: Option<Role>,
    pub critic: Option<Critic>,
    pub repairs: Option<usize>,
    /// What the step reads (git pathspecs): a refresh with none of them changed needs no session.
    pub inputs: Option<Vec<String>>,
    /// What the step writes: read by the hooks, not by the process.
    pub outputs: Option<Vec<String>>,
    /// The worker's tools, by its agent's names: a session starts with only their descriptions. None: every tool.
    pub tools: Option<Vec<String>>,
    /// A command in the process home that does the step by the factory's libraries before its agent, which
    /// gets only what is left.
    pub first: Option<String>,
    /// How the step runs and escalates: its tiers, first to last; none: one agent tier from the settings above.
    pub ladder: Option<Vec<TierSpec>>,
    /// What a human tells the step for every run of it: set once in the project's file, unlike a `--note`.
    pub note: Option<String>,
}

/// The code-quality floor, each field optional so a project's file may replace one.
#[derive(Deserialize, Default, Clone, Debug, PartialEq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Quality {
    pub definition: Option<String>,
    pub version: Option<String>,
    pub min: Option<f64>,
}

/// One layer of settings: the project's file (the process's own are built from its parts).
#[derive(Deserialize, Default, Clone, Debug, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    defaults: StepSettings,
    #[serde(default)]
    steps: HashMap<String, StepSettings>,
    /// the phases and steps switched off, by id; the project's list replaces the process's
    skip: Option<Vec<String>>,
    #[serde(default)]
    quality: Quality,
    #[serde(default)]
    models: HashMap<String, String>,
    /// tools a ladder's decider may pick besides the process's own (a vendor's service the client agreed to)
    managed: Option<Vec<String>>,
    /// the asks the run answers yes itself (a test run): checkpoints by id or name, `task-force`, or `all`
    auto_approve: Option<Vec<String>>,
}

/// The process's settings and the project's overrides.
#[derive(Default, Clone)]
pub struct Settings {
    defaults: StepSettings,
    steps: HashMap<String, StepSettings>,
    skip: Vec<String>,
    quality: Quality,
    models: HashMap<String, String>,
    project: File,
}

impl Settings {
    /// The process's defaults (`defaults`, `skip`, `quality`, `models` of its `settings`, each step's own settings
    /// by id) under the project's file `file` (absent: no overrides), whose keys but `extra` must be known; the
    /// file's text is rendered with `vars`.
    pub fn new(base: Base, file: Option<&Path>, extra: &[String], vars: &BTreeMap<String, String>) -> Result<Settings> {
        let project = match file {
            Some(f) => read(f, extra, vars)?,
            None => File::default(),
        };
        Ok(Settings { defaults: base.defaults, steps: base.steps, skip: base.skip.unwrap_or_default(),
                      quality: base.quality, models: base.models.into_iter().collect(), project })
    }

    /// Whether phase or step `id` is switched off.
    pub fn skipped(&self, id: &str) -> bool {
        self.project.skip.as_ref().unwrap_or(&self.skip).iter().any(|p| p == id)
    }

    /// The model a tier names: a size the project (else the process) maps to a model, or the model itself.
    pub fn model(&self, name: &str) -> String {
        self.project.models.get(name).or_else(|| self.models.get(name)).cloned().unwrap_or_else(|| name.to_string())
    }

    /// The model sizes the project (else the process) maps, sorted: what a decider may pick.
    pub fn sizes(&self) -> Vec<String> {
        let map = if self.project.models.is_empty() { &self.models } else { &self.project.models };
        let mut sizes: Vec<String> = map.keys().cloned().collect();
        sizes.sort();
        sizes
    }

    /// Whether the project has the run answer yes itself to checkpoint `step` (its id) named `name` (`CP3`), or to
    /// the task force's re-entries (`name` = `task-force`). `all` covers both, but never a go-live checkpoint.
    pub fn auto_approved(&self, name: &str, step: &str, go_live: bool) -> bool {
        self.project.auto_approve.as_ref().is_some_and(|a| a.iter().any(|e| e == name || e == step
            || (e == "all" && !go_live)))
    }

    /// The tools the project enabled for a ladder's decider.
    pub fn managed(&self) -> Vec<String> {
        self.project.managed.clone().unwrap_or_default()
    }

    /// The code-quality floor: the project's fields over the process's.
    pub fn quality(&self) -> Quality {
        let (p, f) = (&self.project.quality, &self.quality);
        Quality { definition: p.definition.clone().or_else(|| f.definition.clone()),
                  version: p.version.clone().or_else(|| f.version.clone()), min: p.min.or(f.min) }
    }

    /// The settings of an instance `key` of the step `id` (the same for a step that isn't an instance): the
    /// project's for the key or the id, the project's defaults, the process's for the step, the process's
    /// defaults; the first set one per setting.
    pub fn for_step(&self, key: &str, id: &str) -> StepSettings {
        let layer = |m: &HashMap<String, StepSettings>| m.get(key).or_else(|| m.get(id)).cloned().unwrap_or_default();
        let layers = [layer(&self.project.steps), self.project.defaults.clone(), layer(&self.steps), self.defaults.clone()];
        let worker = merged(layers.iter().filter_map(|l| l.worker.as_ref()));
        let critic = match layers.iter().find_map(|l| l.critic.clone()) {
            Some(Critic::Role(_)) => Some(Critic::Role(merged(layers.iter().filter_map(|l| match &l.critic {
                Some(Critic::Role(r)) => Some(r),
                _ => None,
            })))),
            other => other,
        };
        StepSettings { worker: Some(worker), critic, repairs: layers.iter().find_map(|l| l.repairs),
                       inputs: layers.iter().find_map(|l| l.inputs.clone()),
                       outputs: layers.iter().find_map(|l| l.outputs.clone()),
                       tools: layers.iter().find_map(|l| l.tools.clone()),
                       first: layers.iter().find_map(|l| l.first.clone()),
                       ladder: layers.iter().find_map(|l| l.ladder.clone()),
                       note: layers.iter().find_map(|l| l.note.clone()) }
    }

    /// The ids the project's file names under `steps` (to check them against the process).
    pub fn project_step_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.project.steps.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// The ids the project's `skip` and `auto_approve` name, with where they came from.
    pub fn project_names(&self) -> Vec<(&'static str, String)> {
        let skip = self.project.skip.iter().flatten().map(|s| ("skip", s.clone()));
        let auto = self.project.auto_approve.iter().flatten().map(|s| ("auto_approve", s.clone()));
        skip.chain(auto).collect()
    }
}

/// What the process file itself sets.
#[derive(Default)]
pub struct Base {
    pub defaults: StepSettings,
    pub steps: HashMap<String, StepSettings>,
    pub skip: Option<Vec<String>>,
    pub quality: Quality,
    pub models: BTreeMap<String, String>,
}

impl StepSettings {
    /// Refuses a critic on the worker's agent, where both are set here (a review must be a second opinion).
    pub fn second_opinion(&self, step: &str) -> Result<()> {
        let worker = self.worker.as_ref().and_then(|w| w.agent.as_ref());
        match &self.critic {
            Some(Critic::Role(Role { agent: Some(c), .. })) if Some(c) == worker => {
                Err(anyhow!("step {step}: the critic must use another agent than the worker ({c})"))
            }
            _ => Ok(()),
        }
    }

    /// Whether the step runs without a critic.
    pub fn no_critic(&self) -> bool {
        self.critic == Some(Critic::On(false))
    }

    /// The step with this worker's settings and tools applied over the ones it has.
    pub fn worker_of(&self, step: Step) -> Result<Step> {
        let step = match &self.tools {
            Some(tools) => step.tools(tools.iter().cloned()),
            None => step,
        };
        let Some(r) = &self.worker else { return Ok(step) };
        let step = match &r.agent {
            Some(a) => step.agent(a.parse().map_err(|e: String| anyhow!("worker agent: {e}"))?),
            None => step,
        };
        let step = match &r.effort {
            Some(e) => step.effort(e.parse().map_err(|e: String| anyhow!("worker effort: {e}"))?),
            None => step,
        };
        Ok(match &r.model {
            Some(m) => step.model(m.as_str()),
            None => step,
        })
    }

    /// The critic with this step's critic settings applied over the ones it has.
    pub fn critic_of(&self, judge: Judge) -> Result<Judge> {
        let Some(Critic::Role(r)) = &self.critic else { return Ok(judge) };
        let judge = match &r.agent {
            Some(a) => judge.agent(a.parse().map_err(|e: String| anyhow!("critic agent: {e}"))?),
            None => judge,
        };
        let judge = match &r.effort {
            Some(e) => judge.effort(e.parse().map_err(|e: String| anyhow!("critic effort: {e}"))?),
            None => judge,
        };
        Ok(match &r.model {
            Some(m) => judge.model(m.as_str()),
            None => judge,
        })
    }
}

/// One role from several, the first set value per field.
fn merged<'a>(roles: impl Iterator<Item = &'a Role>) -> Role {
    roles.fold(Role::default(), |acc, r| Role {
        agent: acc.agent.or_else(|| r.agent.clone()),
        model: acc.model.or_else(|| r.model.clone()),
        effort: acc.effort.or_else(|| r.effort.clone()),
    })
}

/// The project's file, or no settings when it doesn't exist: rendered with `vars`, its keys but `extra` known.
fn read(path: &Path, extra: &[String], vars: &BTreeMap<String, String>) -> Result<File> {
    if !path.exists() {
        return Ok(File::default());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let text = crate::core::vars::render(&text, vars).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    let mut value: serde_yaml::Value = serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if let Some(map) = value.as_mapping_mut() {
        for key in extra {
            map.remove(key.as_str());
        }
    }
    if value.is_null() {
        return Ok(File::default());
    }
    serde_yaml::from_value(value).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Base {
        let mut b = Base { defaults: serde_yaml::from_str("{ repairs: 2, worker: { effort: medium } }").unwrap(), ..Base::default() };
        b.steps.insert("knowledge".into(), serde_yaml::from_str("{ critic: false, worker: { effort: low } }").unwrap());
        b.steps.insert("transform".into(), serde_yaml::from_str("{ repairs: 5 }").unwrap());
        b.skip = Some(vec!["release".into()]);
        b
    }

    /// The project's step setting wins over its defaults, those over the process's step, then the process's.
    #[test]
    fn a_setting_comes_from_the_first_layer_that_sets_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("project.yaml");
        std::fs::write(&file, "steps:\n  transform: { worker: { model: m1 }, critic: { effort: high } }\n  \
            audit: { note: \"profile quick\" }\nmanaged: [vendor]\nextra_thing: 1\n").unwrap();
        let s = Settings::new(base(), Some(&file), &["extra_thing".into()], &BTreeMap::new()).unwrap();
        let knowledge = s.for_step("knowledge", "knowledge");
        assert!(knowledge.no_critic() && knowledge.repairs == Some(2));
        assert_eq!(knowledge.worker.unwrap().effort.as_deref(), Some("low"));
        let unit = s.for_step("transform-U1", "transform");
        assert_eq!(unit.repairs, Some(5), "an instance has its step's settings");
        let worker = unit.worker.clone().unwrap();
        assert_eq!((worker.model.as_deref(), worker.effort.as_deref()), (Some("m1"), Some("medium")));
        assert!(!unit.no_critic() && matches!(unit.critic, Some(Critic::Role(Role { effort: Some(_), .. }))));
        assert_eq!(s.for_step("audit", "audit").note.as_deref(), Some("profile quick"));
        assert_eq!(s.for_step("transform", "transform").note, None);
        assert_eq!(s.managed(), ["vendor"]);
        assert!(Settings::new(base(), Some(&file), &[], &BTreeMap::new()).is_err(), "an unknown key is an error");
        assert!(Settings::new(Base::default(), Some(&dir.path().join("absent.yaml")), &[], &BTreeMap::new()).unwrap()
            .for_step("a", "a").critic.is_none());
    }

    /// Skips, quality, models and auto-approval layer the project over the process.
    #[test]
    fn skip_quality_models_and_auto_approval_layer_over_the_process() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("project.yaml");
        let mut b = base();
        b.quality = Quality { definition: Some("d".into()), version: Some("1".into()), min: Some(90.0) };
        b.models.insert("small".into(), "m-small".into());
        let s = Settings::new(b, None, &[], &BTreeMap::new()).unwrap();
        assert!(s.skipped("release") && !s.skipped("verify"));
        assert_eq!((s.model("small"), s.model("other"), s.sizes()), ("m-small".into(), "other".into(), vec!["small".to_string()]));
        assert!(!s.auto_approved("CP3", "design", false), "humans answer unless the project says otherwise");
        std::fs::write(&file, "skip: []\nquality: { min: 85 }\nauto_approve: [design, CP4]\n").unwrap();
        let mut b = base();
        b.quality = Quality { definition: Some("d".into()), version: Some("1".into()), min: Some(90.0) };
        let s = Settings::new(b, Some(&file), &[], &BTreeMap::new()).unwrap();
        assert!(!s.skipped("release"), "a project switches it on");
        let q = s.quality();
        assert_eq!((q.min, q.version.as_deref()), (Some(85.0), Some("1")), "a project replaces one field");
        assert!(s.auto_approved("CP3", "design", false) && s.auto_approved("CP4", "plan", false), "by id or name");
        assert!(!s.auto_approved("CP6", "release", false) && !s.auto_approved("task-force", "", false));
        std::fs::write(&file, "auto_approve: [all]\n").unwrap();
        let all = Settings::new(base(), Some(&file), &[], &BTreeMap::new()).unwrap();
        assert!(all.auto_approved("CP6", "release", false) && all.auto_approved("task-force", "", false));
        assert!(!all.auto_approved("CP7", "go-live", true), "a go-live only by its id or name");
    }
}
