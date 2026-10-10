//! `agg check` — validate `agg.yaml` and the judges it names exactly as `agg run` does at startup,
//! and report EVERY problem at once, each at its dotted config path. Runs no judge and no agent
//! (the only subprocess is each agent CLI's `--version`, the same probe startup makes).
//!
//! Startup refuses at the first error of each stage; that is right for a run and wrong for a
//! config somebody (or something) GENERATED, which wants the whole list in one pass. So every
//! stage here collects: the parse (all unknown keys via the schema, then the rest), the assembly
//! (steps, judges, expressions), the capability check, and the agent preflight. A stage still
//! runs when an earlier one found problems, as long as there is a config to run it on.

use crate::assembly::{self, Assembly};
use crate::core::config::AggConfig;
use crate::core::model::Judge;
use crate::problem::{self, Problem};
use serde::Serialize;
use serde_yaml::Value as Yaml;
use std::path::Path;

/// Bumped on any breaking change to the JSON of `agg check --json` / `agg doctor --json`.
pub const SCHEMA_VERSION: u32 = 1;

/// The machine-readable result of `agg check --json`.
#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub schema_version: u32,
    /// no problems (warnings do not count).
    pub ok: bool,
    /// the config file that was checked.
    pub config: String,
    pub problems: Vec<Problem>,
    /// non-fatal: a guard that is inert on an agent that still caps itself.
    pub warnings: Vec<Problem>,
}

/// Everything startup validation learned — the config and assembly when they could be built,
/// and the findings either way. `agg doctor` builds on this rather than repeating it.
#[derive(Default)]
pub struct Validation {
    pub cfg: Option<AggConfig>,
    pub assembly: Option<Assembly>,
    pub problems: Vec<Problem>,
    pub warnings: Vec<Problem>,
    /// the container images the run uses, as the startup probe found them.
    pub images: Vec<crate::isolation::probe::ImageFacts>,
}

impl Validation {
    /// The JSON-ready report for `config`.
    pub fn report(&self, config: &Path) -> CheckReport {
        CheckReport {
            schema_version: SCHEMA_VERSION,
            ok: self.problems.is_empty(),
            config: config.display().to_string(),
            problems: self.problems.clone(),
            warnings: self.warnings.clone(),
        }
    }
}

/// Run every startup check on `config` (judges resolved against `config_base`), collecting all
/// problems.
pub fn validate(config_base: &Path, config: &Path) -> Validation {
    let mut v = Validation::default();
    let Some(mut cfg) = parse(config, &mut v.problems) else { return v };
    cfg.finish_load();

    let judges: Vec<Judge> = match assembly::build(&cfg, config_base) {
        Ok(asm) => {
            let j = asm.engine.judges.clone();
            v.assembly = Some(asm);
            j
        }
        Err(r) => {
            v.problems.extend(r.problems);
            r.judges
        }
    };

    let project = config_base.parent().unwrap_or(config_base);
    let a = crate::capability::assess(&cfg, &judges, project);
    v.problems.extend(a.unknown_agents);
    // the swe-metrics probe (assembly) and the image probe (here) both report an image without it.
    for p in a.problems {
        problem::push_unique(&mut v.problems, p);
    }
    v.warnings = a.warnings;
    v.images = a.images;

    // `agg run`'s last startup check: every agent that runs on the HOST is on PATH (a container
    // step's agent is checked in its image, by the probe above).
    for (path, name) in crate::capability::host_agents(&cfg, &judges) {
        if let Ok(b) = crate::backend::for_name(&name) {
            if let Err(e) = b.preflight() {
                problem::push_unique(&mut v.problems, Problem::new(path, format!("{e:#}")));
            }
        }
    }
    v.cfg = Some(cfg);
    v
}

/// Each distinct agent name with the FIRST key that names it (`defaults.agent`, `judge.agent`,
/// `steps.<name>.agent`), so a missing CLI is reported once, where it is chosen.
pub fn agent_sites(cfg: &AggConfig) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![
        ("defaults.agent".into(), cfg.defaults.agent.clone()),
        ("judge.agent".into(), cfg.judge.agent.clone()),
    ];
    for (step, body) in &cfg.steps {
        if let Some(a) = &body.agent {
            out.push((format!("steps.{step}.agent"), a.clone()));
        }
    }
    let mut seen: Vec<String> = Vec::new();
    out.retain(|(_, n)| {
        let new = !seen.contains(n);
        seen.push(n.clone());
        new
    });
    out
}

/// Read + parse the config, listing EVERY parse problem. Returns the config when one can be had —
/// including when the only problems were unknown keys, which are then set aside so the later
/// stages still run.
fn parse(config: &Path, problems: &mut Vec<Problem>) -> Option<AggConfig> {
    let text = match std::fs::read_to_string(config) {
        Ok(t) => t,
        Err(e) => {
            let why = if config.exists() { format!("reading {}: {e}", config.display()) } else {
                format!("no {} here — run `agg init` to scaffold one", config.display())
            };
            problems.push(Problem::new("", why));
            return None;
        }
    };
    let doc: Option<Yaml> = serde_yaml::from_str(&text).ok();
    if let Err(e) = AggConfig::reject_removed_keys(config) {
        // a migration error, and the one startup gives FIRST: what follows it would be noise.
        problems.push(Problem::new(doc.as_ref().map(removed_key_path).unwrap_or_default(), format!("{e:#}")));
        return None;
    }
    let first = match serde_path_to_error::deserialize::<_, AggConfig>(serde_yaml::Deserializer::from_str(&text)) {
        Ok(cfg) => return Some(cfg),
        Err(e) => e,
    };
    let Some(mut doc) = doc else {
        problems.push(Problem::new(dotted(first.path()), first.inner().to_string()));
        return None;
    };
    // EVERY unknown key at once (serde stops at the first), each set aside so the parse can go on.
    let unknown = crate::schema::unknown_keys(&doc);
    if unknown.is_empty() {
        // not a key problem: keep serde's own startup message (it carries the line and column).
        let at = dotted(first.path());
        problems.push(Problem::new(at.clone(), first.inner().to_string()));
        if !remove_at(&mut doc, &at) {
            return None;
        }
    }
    for p in &unknown {
        remove_at(&mut doc, &p.path);
    }
    problems.extend(unknown);
    // Each further value serde refuses is reported and set aside too (its key falls back to its
    // default), so one bad value does not hide the problems behind it. Bounded: every round
    // removes a key, and a refusal that names no removable key ends it.
    for _ in 0..MAX_PARSE_ROUNDS {
        match serde_path_to_error::deserialize::<_, AggConfig>(doc.clone()) {
            Ok(cfg) => return Some(cfg),
            Err(e) => {
                let at = dotted(e.path());
                problems.push(Problem::new(at.clone(), e.inner().to_string()));
                if !remove_at(&mut doc, &at) {
                    return None;
                }
            }
        }
    }
    None
}

/// How many refused values one `agg check` sets aside before giving up on the parse.
const MAX_PARSE_ROUNDS: usize = 64;

/// `serde_path_to_error`'s path in our spelling: the root (`.`) is the empty string.
fn dotted(p: &serde_path_to_error::Path) -> String {
    match p.to_string().as_str() {
        "." => String::new(),
        s => s.to_string(),
    }
}

/// Where a removed `wall_hours` sits: the `limits` key, else the expression that uses it.
fn removed_key_path(doc: &Yaml) -> String {
    let seq = &doc["sequence"];
    if !seq["limits"]["wall_hours"].is_null() {
        return "sequence.limits.wall_hours".into();
    }
    ["done_if", "abort_if", "notify_if"]
        .iter()
        .find(|k| seq[**k].as_str().is_some_and(|e| e.contains("wall_hours")))
        .map(|k| format!("sequence.{k}"))
        .unwrap_or_else(|| "sequence".into())
}

/// Delete the key at a dotted path (`a.b[1].c`) from a YAML document. Returns whether a key was
/// removed; a path that does not lead to a mapping key (the root, a list item) is left alone.
fn remove_at(doc: &mut Yaml, path: &str) -> bool {
    let (parent, key) = path.rsplit_once('.').unwrap_or(("", path));
    let mut cur = doc;
    for seg in parent.split('.').filter(|s| !s.is_empty()) {
        let (name, idxs) = seg.split_once('[').unwrap_or((seg, ""));
        let Some(next) = cur.get_mut(name) else { return false };
        cur = next;
        for i in idxs.split('[').filter_map(|s| s.trim_end_matches(']').parse::<usize>().ok()) {
            let Some(next) = cur.get_mut(i) else { return false };
            cur = next;
        }
    }
    match cur {
        Yaml::Mapping(m) => m.remove(key).is_some(),
        _ => false,
    }
}

/// `agg check [--json]`: print the result and return whether the config is valid.
pub fn run(config_base: &Path, config: &Path, json: bool) -> anyhow::Result<bool> {
    let v = validate(config_base, config);
    let r = v.report(config);
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(r.ok);
    }
    eprintln!("agg check — {}", config.display());
    for w in &r.warnings {
        eprintln!("  ⚠ {w}");
    }
    for p in &r.problems {
        eprintln!("  ✗ {p}");
    }
    match (&v.assembly, r.ok) {
        (Some(a), true) => eprintln!("✔ valid — {} judge(s) resolve; nothing was run.", a.engine.judges.len()),
        _ => eprintln!("✗ {} problem(s) — `agg run` would refuse to start.", r.problems.len()),
    }
    Ok(r.ok)
}

#[cfg(test)]
mod tests;
