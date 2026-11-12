//! The numbers humans read (A-37 §4.5), computed from position and never written in the file: a phase's number is
//! its index, a step's `<phase>.<k>` (every kind counted; an `each`'s children numbered in its place), and a
//! checkpoint's name `CP<k>` by its place among the checkpoints. A step's identity is its id.

use serde_json::json;

use super::format::{Kind, Process, Step};

/// One step of the numbered process.
#[derive(Debug, Clone)]
pub struct Numbered {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub phase_id: String,
    pub phase_name: String,
    pub phase_number: usize,
    /// `None` for an `each` itself
    pub number: Option<String>,
    /// `CP<k>` for a checkpoint
    pub cp: Option<String>,
    /// the id of the `each` this is a child of
    pub parent: Option<String>,
    pub meta: Option<serde_yaml::Value>,
}

impl Numbered {
    /// `<number> <name>` (a checkpoint's `<number> CP<k> <name>`); none for an `each`.
    pub fn label(&self) -> Option<String> {
        let number = self.number.as_ref()?;
        Some(match &self.cp {
            Some(cp) => format!("{number} {cp} {}", self.name),
            None => format!("{number} {}", self.name),
        })
    }

    /// The label of the instance of this step for the item `key` of its `each`.
    pub fn instance_label(&self, key: &str) -> Option<String> {
        let number = self.number.as_ref()?;
        Some(format!("{number}-{key} {}", self.name))
    }

    /// The `--list` entry.
    pub fn json(&self) -> serde_json::Value {
        let mut v = json!({"id": self.id, "phase": self.phase_id, "phase_name": self.phase_name,
                           "phase_number": self.phase_number, "number": self.number, "name": self.name,
                           "label": self.label(), "kind": format!("{:?}", self.kind).to_lowercase()});
        if let Some(cp) = &self.cp {
            v["cp"] = json!(cp);
        }
        if let Some(parent) = &self.parent {
            v["each"] = json!(parent);
        }
        if let Some(meta) = &self.meta {
            v["meta"] = serde_json::to_value(meta).unwrap_or_default();
        }
        v
    }
}

/// Every step in order: an `each` first, then its children.
pub fn number(process: &Process) -> Vec<Numbered> {
    let mut out = Vec::new();
    let mut cps = 0;
    for (i, phase) in process.phases.iter().enumerate() {
        let mut k = 0;
        for step in &phase.steps {
            add(&mut out, step, (i + 1, &phase.id, &phase.name), &mut k, &mut cps, None);
        }
    }
    out
}

fn add(out: &mut Vec<Numbered>, step: &Step, phase: (usize, &str, &str), k: &mut usize, cps: &mut usize,
       parent: Option<&str>) {
    let (n, phase_id, phase_name) = phase;
    let each = step.kind == Kind::Each;
    if !each {
        *k += 1;
    }
    let cp = (step.kind == Kind::Checkpoint).then(|| {
        *cps += 1;
        format!("CP{cps}")
    });
    out.push(Numbered { id: step.id.clone(), name: step.name.clone(), kind: step.kind, phase_id: phase_id.into(),
                        phase_name: phase_name.into(), phase_number: n, number: (!each).then(|| format!("{n}.{k}")), cp,
                        parent: parent.map(String::from), meta: step.meta.clone() });
    for child in &step.steps {
        add(out, child, phase, k, cps, Some(&step.id));
    }
}

/// The key a `--from`/`--until` argument names: an id; an instance key (`<child id>-<item>`); a number (`3.4`)
/// or an instance's number (`6.3-U1`), turned into the id or instance key.
pub fn resolve(list: &[Numbered], text: &str) -> Result<String, String> {
    if list.iter().any(|s| s.id == text && s.kind != Kind::Each && s.number.is_some()) {
        return Ok(text.to_string());
    }
    let (number, item) = match text.split_once('-') {
        Some((n, item)) if n.chars().next().is_some_and(|c| c.is_ascii_digit()) => (n, Some(item)),
        _ => (text, None),
    };
    if let Some(step) = list.iter().find(|s| s.number.as_deref() == Some(number)) {
        return match (item, &step.parent) {
            (None, _) => Ok(step.id.clone()),
            (Some(item), Some(_)) => Ok(format!("{}-{item}", step.id)),
            (Some(_), None) => Err(format!("step {number} isn't a step of an `each`: no instance `{text}`")),
        };
    }
    let child = list.iter().filter(|s| s.parent.is_some())
        .filter(|s| text.strip_prefix(s.id.as_str()).is_some_and(|rest| rest.starts_with('-') && rest.len() > 1))
        .max_by_key(|s| s.id.len());
    if child.is_some() {
        return Ok(text.to_string());
    }
    Err(format!("`{text}` names no step; the closest ids: {}", closest(list, text).join(", ")))
}

/// The ids and numbers nearest to `text` by shared prefix, at most five.
fn closest(list: &[Numbered], text: &str) -> Vec<String> {
    let shared = |a: &str| a.chars().zip(text.chars()).take_while(|(x, y)| x == y).count();
    let mut ids: Vec<(usize, String)> = list.iter().filter(|s| s.number.is_some()).map(|s| (shared(&s.id), s.id.clone())).collect();
    ids.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ids.into_iter().take(5).map(|(_, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process() -> Process {
        Process::parse("version: 1\nphases:\n\
            - id: one\n  name: One\n  steps:\n\
            \x20 - { id: a, name: Alpha, kind: outside }\n\
            \x20 - { id: b, name: Beta, kind: script, run: x }\n\
            \x20 - { id: cp-b, name: B approved, kind: checkpoint, prepare: p, doc: d.md, question: q }\n\
            - id: two\n  name: Two\n  steps:\n\
            \x20 - { id: units, name: Units, kind: each, items: { file: plan.yaml }, steps: [\
            { id: build, name: Build, kind: agent }, { id: merge, name: Merge, kind: script, run: m }] }\n\
            \x20 - { id: later, name: Later, kind: planned }\n\
            \x20 - { id: cp-later, name: Later approved, kind: checkpoint, prepare: p, doc: d2.md, question: q }\n").unwrap()
    }

    /// Phases and steps count from 1; an `each` takes no number and its children count in its place; checkpoints
    /// are named by their place among the checkpoints.
    #[test]
    fn numbers_and_checkpoint_names_come_from_position() {
        let list = number(&process());
        let labels: Vec<Option<String>> = list.iter().map(Numbered::label).collect();
        assert_eq!(labels, [Some("1.1 Alpha".into()), Some("1.2 Beta".into()), Some("1.3 CP1 B approved".into()),
                            None, Some("2.1 Build".into()), Some("2.2 Merge".into()), Some("2.3 Later".into()),
                            Some("2.4 CP2 Later approved".into())]);
        assert_eq!(list[4].instance_label("U1").as_deref(), Some("2.1-U1 Build"));
        assert_eq!(list[4].parent.as_deref(), Some("units"));
        assert_eq!(list[7].json()["cp"], "CP2");
    }

    /// `--from` takes an id, a number, an instance key or an instance's number; anything else names the closest ids.
    #[test]
    fn a_step_is_named_by_id_number_or_instance() {
        let list = number(&process());
        let to = |t: &str| resolve(&list, t);
        assert_eq!((to("b").unwrap(), to("1.2").unwrap(), to("2.2").unwrap()), ("b".into(), "b".into(), "merge".into()));
        assert_eq!(to("build-U1").unwrap(), "build-U1");
        assert_eq!(to("2.1-U7").unwrap(), "build-U7");
        assert!(to("1.2-U1").unwrap_err().contains("isn't a step of an `each`"));
        assert!(to("units").is_err(), "an `each` is no step to start at");
        let err = to("bet").unwrap_err();
        assert!(err.contains("names no step") && err.contains("b,"), "{err}");
    }
}
