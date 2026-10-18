//! Roles from configuration (A-31): the few roles a process uses across its steps (a worker, a reviewer on
//! another agent), each an agent, a model and an effort chosen in one file.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Context as _};
use serde::Deserialize;

use crate::core::model::Judge;
use crate::driver::{Agent, Effort, Step};

/// One role as the file names it.
#[derive(Deserialize)]
struct Named {
    agent: String,
    model: Option<String>,
    effort: Option<String>,
}

/// A role: its agent, model (none: the agent's own) and effort.
#[derive(Debug, Clone)]
pub struct Role {
    pub agent: Agent,
    pub model: Option<String>,
    pub effort: Effort,
}

/// The roles a process uses, by name.
#[derive(Debug, Clone)]
pub struct Roles(BTreeMap<String, Role>);

impl Roles {
    /// Reads the roles `names` from the top-level keys of the YAML `file` (other keys are left alone); a
    /// missing role, an unknown agent or effort, or an agent that isn't installed is an error.
    pub fn load(file: &Path, names: &[&str]) -> anyhow::Result<Roles> {
        let text = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let roles = Roles::parse(&text, names).with_context(|| format!("in {}", file.display()))?;
        for (name, role) in &roles.0 {
            crate::backend::for_name(role.agent.name())?.preflight().with_context(|| format!("role `{name}`"))?;
        }
        Ok(roles)
    }

    /// The roles `names` from YAML `text`, unchecked against the machine.
    pub fn parse(text: &str, names: &[&str]) -> anyhow::Result<Roles> {
        let all: BTreeMap<String, serde_yaml::Value> = serde_yaml::from_str(text)?;
        let mut roles = BTreeMap::new();
        for name in names {
            let value = all.get(*name).ok_or_else(|| anyhow!("no role `{name}`"))?;
            let named: Named = serde_yaml::from_value(value.clone()).with_context(|| format!("role `{name}`"))?;
            let agent = named.agent.parse().map_err(|e: String| anyhow!("role `{name}`: {e}"))?;
            let effort = match named.effort {
                Some(e) => e.parse().map_err(|e: String| anyhow!("role `{name}`: {e}"))?,
                None => Effort::Default,
            };
            roles.insert(name.to_string(), Role { agent, model: named.model, effort });
        }
        Ok(Roles(roles))
    }

    /// The role `name`; panics when it wasn't loaded (a driver names only roles it loaded).
    pub fn get(&self, name: &str) -> &Role {
        self.0.get(name).unwrap_or_else(|| panic!("role `{name}` was not loaded"))
    }

    /// A step named `name` on role `role`, with its task in `prompt_file`.
    pub fn step(&self, role: &str, name: &str, prompt_file: impl Into<String>) -> Step {
        let r = self.get(role);
        let step = Step::new(name).agent(r.agent.clone()).effort(r.effort).prompt_file(prompt_file);
        match &r.model {
            Some(m) => step.model(m.as_str()),
            None => step,
        }
    }

    /// A rubric judge named `name` on role `role`, with its criteria in `rubric`.
    pub fn judge(&self, role: &str, name: &str, rubric: impl Into<std::path::PathBuf>) -> Judge {
        let r = self.get(role);
        let judge = Judge::rubric(name, rubric).agent(r.agent.clone()).effort(r.effort);
        match &r.model {
            Some(m) => judge.model(m.as_str()),
            None => judge,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two roles set a step's and a judge's agent, model and effort; other keys stay unread.
    #[test]
    fn roles_set_the_steps_and_the_judges_agent_model_and_effort() {
        let text = "worker: { agent: claude, model: m1, effort: high }\ncritic: { agent: codex }\ntools: [x]\n";
        let roles = Roles::parse(text, &["worker", "critic"]).unwrap();
        let step = roles.step("worker", "1 Do", "/x/do.md");
        assert_eq!((step.agent.name(), step.model.as_deref(), step.effort), ("claude", Some("m1"), Effort::High));
        assert_eq!(roles.get("critic").agent.name(), "codex");
        assert!(Roles::parse(text, &["planner"]).unwrap_err().to_string().contains("no role `planner`"));
        assert!(Roles::parse("worker: { agent: gpt }\n", &["worker"]).is_err());
    }
}
