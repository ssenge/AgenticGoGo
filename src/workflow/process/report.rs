//! The run report (A-38 3.7): where a process run is (`map`), when each step ended (`timeline`) and where the time
//! went (`profile`, in [`super::profile`]), computed from the numbering, the run record, the call log and the asks.
//! `steps[].extra` is left empty for a caller's own facts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use super::format::{Kind, Process};
use super::number::{number, Numbered};
use super::{run, template};

/// Result kinds that say a step's work was done.
const DONE: [&str; 7] = ["done", "tool", "unchanged", "approval", "auto-approval", "accepted", "checkpoint"];
/// The kinds of a step's record lines that end an attempt.
const RESULTS: [&str; 10] = ["done", "tool", "unchanged", "approval", "auto-approval", "accepted", "checkpoint", "skipped",
                             "task-force", "unmet"];
/// Lines about a step, not the end of one.
pub const NOT_RESULTS: [&str; 2] = ["hook", "repair"];

/// A process run on a project: everything a report reads.
pub struct Run {
    pub project: PathBuf,
    pub list: Vec<Numbered>,
    pub spec: Process,
    pub record: Vec<Value>,
    off: Vec<String>,
    units: BTreeMap<String, Vec<String>>,
    /// the step the live run is in, and whether the run has ended (a stopped run is to be resumed)
    pub current: Option<String>,
    pub ended: bool,
}

/// One step of the map: its place, and what the run did with it.
struct Place {
    key: String,
    number: String,
    name: String,
    phase: usize,
    kind: Kind,
    parent: Option<String>,
    item: Option<String>,
}

impl Run {
    /// The run of `project` under the process in `file`: its record, state and settings.
    pub fn open(file: &Path, project: &Path) -> Result<Run> {
        let text = std::fs::read_to_string(file).map_err(|e| anyhow!("{}: {e}", file.display()))?;
        let spec = Process::parse(&text).map_err(|e| anyhow!("{}: {e}", file.display()))?;
        let list = number(&spec);
        let project = std::fs::canonicalize(project).map_err(|e| anyhow!("{}: {e}", project.display()))?;
        let home = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let vars = template::variables(&spec, &list, &home, &project.display().to_string()).map_err(|e| anyhow!("{e}"))?;
        let history = spec.run.history.clone().unwrap_or_else(|| crate::workflow::history::DEFAULT.into());
        let record = lines(&project.join(history));
        let (current, ended) = live(&project);
        let mut run = Run { off: vec![], units: BTreeMap::new(), project, list, spec, record, current, ended };
        run.units = run.items(&vars);
        run.off = run.switched_off(&home, &vars)?;
        Ok(run)
    }

    /// The items of each `each`, by its id; none where its file isn't there yet.
    fn items(&self, vars: &BTreeMap<String, String>) -> BTreeMap<String, Vec<String>> {
        let mut out = BTreeMap::new();
        for step in self.spec.phases.iter().flat_map(|p| p.steps.iter()).filter(|s| s.kind == Kind::Each) {
            let Some(spec) = &step.items else { continue };
            let Ok(file) = template::render(&spec.file, vars) else { continue };
            let doc: serde_yaml::Value = std::fs::read_to_string(self.project.join(file)).ok()
                .and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
            let key = spec.key.as_deref().unwrap_or("id");
            let all = doc.get(spec.items.as_deref().unwrap_or("units")).and_then(|v| v.as_sequence()).cloned().unwrap_or_default();
            let ids: Vec<String> = all.iter().filter_map(|v| v.get(key)?.as_str().map(String::from)).collect();
            let order: Vec<String> = match spec.list.as_deref() {
                Some(l) => doc.get(l).and_then(|v| v.as_sequence()).into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect(),
                None => ids,
            };
            out.insert(step.id.clone(), order);
        }
        out
    }

    /// The ids of the phases and steps switched off: the effective `skip` (the project's settings, else the
    /// process's) and the run's configuration file's `switched_off`.
    fn switched_off(&self, home: &Path, vars: &BTreeMap<String, String>) -> Result<Vec<String>> {
        // a project's settings the process no longer fits (an old project) must not hide the report: the process's own skip then
        let settings = run::settings_of(&self.spec, &self.list, home, &self.project, vars).ok();
        let skipped = |id: &String| match &settings {
            Some(s) => s.skipped(id),
            None => self.spec.settings.skip.as_ref().is_some_and(|l| l.contains(id)),
        };
        let mut off: Vec<String> = self.spec.phases.iter().map(|p| p.id.clone()).chain(self.list.iter().map(|n| n.id.clone()))
            .filter(skipped).collect();
        if let Some(file) = &self.spec.run.config {
            let run: serde_yaml::Value = std::fs::read_to_string(self.project.join(file)).ok()
                .and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
            if let Some(map) = run.get("switched_off").and_then(|m| m.as_mapping()) {
                off.extend(map.keys().filter_map(|k| k.as_str().map(String::from)));
            }
        }
        Ok(off)
    }

    /// Every step in the process's order, an `each`'s children once per item; and any step the record has that the
    /// process lacks isn't added (the record's ids are the process's).
    fn places(&self) -> Vec<Place> {
        let mut out = vec![];
        for n in self.list.iter().filter(|n| n.kind != Kind::Each) {
            let Some(num) = &n.number else { continue };
            match &n.parent {
                None => out.push(Place { key: n.id.clone(), number: num.clone(), name: n.name.clone(), phase: n.phase_number, kind: n.kind,
                                         parent: None, item: None }),
                Some(each) => out.extend(self.units.get(each).into_iter().flatten().map(|item| Place {
                    key: format!("{}-{item}", n.id), number: format!("{num}-{item}"), item: Some(item.clone()),
                    name: n.name.clone(), phase: n.phase_number, kind: n.kind, parent: Some(each.clone()) })),
            }
        }
        out
    }

    /// The key a record entry is of: its `id`, else its step's number resolved (an entry written before ids).
    fn key_of(&self, entry: &Value) -> String {
        if let Some(id) = entry["id"].as_str() {
            return id.to_string();
        }
        let word = entry["step"].as_str().unwrap_or_default().split(' ').next().unwrap_or_default();
        super::number::resolve(&self.list, word).unwrap_or_else(|_| word.to_string())
    }

    /// The record's entries by step key.
    fn by_key(&self) -> BTreeMap<String, Vec<Value>> {
        let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for e in &self.record {
            out.entry(self.key_of(e)).or_default().push(e.clone());
        }
        out
    }

    /// Whether the step's phase, itself or its `each` is switched off.
    fn is_off(&self, place: &Place) -> bool {
        let phase = self.spec.phases.get(place.phase - 1).map(|p| p.id.as_str()).unwrap_or_default();
        let base = place.key.split('-').next().unwrap_or_default();
        let names = [Some(phase), Some(place.key.as_str()), Some(base), place.parent.as_deref()];
        names.iter().flatten().any(|n| self.off.iter().any(|o| o == n)) || matches!(place.kind, Kind::Outside | Kind::Planned)
    }

    /// The map: per phase its steps with their states and, for a checkpoint, its answer.
    pub fn map(&self) -> Value {
        let places = self.places();
        let by = self.by_key();
        let running = self.current.as_ref().map(|c| c.split(' ').next().unwrap_or(c).to_string())
            .and_then(|c| super::number::resolve(&self.list, &c).ok().or(Some(c)));
        let keys: Vec<&str> = places.iter().map(|p| p.key.as_str()).collect();
        let end = passed_until(&keys, running.as_deref(), &by, self.ended);
        let mut phases: Vec<Value> = self.spec.phases.iter().enumerate()
            .map(|(i, p)| json!({"n": i + 1, "id": p.id, "name": p.name, "off": self.off.contains(&p.id), "steps": []})).collect();
        for (i, p) in places.iter().enumerate() {
            let state = state_of(by.get(&p.key).map(Vec::as_slice).unwrap_or_default());
            let shown = placed(state["state"].as_str().unwrap_or("ahead"), running.as_deref() == Some(p.key.as_str()), self.is_off(p), i < end);
            let mut step = json!({"id": p.key, "n": p.number, "name": p.name, "kind": format!("{:?}", p.kind).to_lowercase(),
                                  "extra": {}, "runs": state["runs"], "failures": state["failures"], "repairs": state["repairs"],
                                  "state": shown, "answer": state["answer"]});
            if let Some(item) = &p.item {
                step["item"] = json!(item);
            }
            if let Some(s) = phases[p.phase - 1]["steps"].as_array_mut() {
                s.push(step);
            }
        }
        json!({"phases": phases, "current": running, "ended": self.ended})
    }

    /// The timeline: one row per step result with its minutes, and the run's totals.
    pub fn timeline(&self) -> Value {
        let rows = steps(&self.record);
        let paused: f64 = rows.iter().map(|r| r["paused"].as_f64().unwrap_or(0.0)).sum();
        let wall: f64 = rows.iter().map(|r| r["minutes"].as_f64().unwrap_or(0.0)).sum::<f64>() + paused;
        let waiting: f64 = rows.iter().filter(|r| asked(r)).map(|r| r["minutes"].as_f64().unwrap_or(0.0)).sum();
        let count = |f: &dyn Fn(&Value) -> bool| rows.iter().filter(|r| f(r)).count();
        json!({"steps": rows, "wall": wall, "waiting": waiting, "paused": paused,
               "repairs": rows.iter().map(|r| r["repairs"].as_u64().unwrap_or(0)).sum::<u64>(),
               "task_force": count(&|r| r["kind"] == "task-force"), "checkpoints_asked": count(&|r| asked(r))})
    }
}

/// The run of the process a project last ran (`agg/private/process.json`); none when it never ran one.
pub fn of_project(project: &Path) -> Option<Run> {
    let note: Value = serde_json::from_str(&std::fs::read_to_string(crate::paths::private_dir(project).join("process.json")).ok()?).ok()?;
    Run::open(Path::new(note["file"].as_str()?), project).ok()
}

/// What `kind` (`map`, `timeline` or `profile`) says of the project's process run; none for another kind or no run.
pub fn of(project: &Path, kind: &str) -> Option<Value> {
    kind_of(&of_project(project)?, kind)
}

/// What `kind` says of the run of `project` under the process in `file` (a project that noted no process file of its
/// own: `agg serve --process`); none for another kind or a file that doesn't open.
pub fn of_file(file: &Path, project: &Path, kind: &str) -> Option<Value> {
    kind_of(&Run::open(file, project).ok()?, kind)
}

/// The report of `kind` of a run.
fn kind_of(run: &Run, kind: &str) -> Option<Value> {
    match kind {
        "map" => Some(run.map()),
        "timeline" => Some(run.timeline()),
        "profile" => Some(super::profile::profile(run)),
        _ => None,
    }
}

/// The JSON lines of a file; none when it isn't there; a torn line is skipped.
pub fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The step a live run is in and whether the run has ended (`agg/private/state.json`; no such file: ended).
fn live(project: &Path) -> (Option<String>, bool) {
    let state: Value = std::fs::read_to_string(project.join("agg/private/state.json")).ok()
        .and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let finished = state["finished"].as_bool().unwrap_or(true);
    (state["step"].as_str().filter(|s| !finished && !s.is_empty()).map(String::from), finished)
}

/// What the record says of one step: its state, how often it ran, its repairs, a checkpoint's answer.
pub fn state_of(entries: &[Value]) -> Value {
    let entries = replayed_out(entries);
    let kind = |e: &Value| e["kind"].as_str().unwrap_or_default().to_string();
    let kinds: Vec<String> = entries.iter().map(kind).collect();
    let count = |k: &str| kinds.iter().filter(|x| *x == k).count();
    let results: Vec<&Value> = entries.iter().filter(|e| RESULTS.contains(&kind(e).as_str())).collect();
    let failures = results.iter().filter(|e| failed(e)).count();
    let answers: Vec<&str> = entries.iter().filter_map(|e| match kind(e).as_str() {
        "approval" => Some(if e["met"] == true { "yes" } else { "no" }),
        "auto-approval" => Some("auto"),
        _ => None,
    }).collect();
    let redone = count("done") > 1 || failures > 0 || count("reentry") > 0 || answers.len() > 1;
    json!({"state": label(results.last().copied(), redone, count("repair") > 0), "runs": count("done"), "failures": failures,
           "repairs": count("repair"), "answer": answers.last()})
}

/// An attempt that failed: an unmet result, or one the task force sent back (its own line is met: it held).
fn failed(result: &Value) -> bool {
    result["met"] == false || result["kind"] == "task-force"
}

/// A step's record lines without a resumed run's replay: on `--resume` each step the run fast-forwards is
/// recorded again with the same result.
fn replayed_out(entries: &[Value]) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    entries.iter().filter(|e| {
        let key = (e["kind"].to_string(), e["met"].to_string(), e["rationale"].to_string());
        let replay = DONE.contains(&e["kind"].as_str().unwrap_or_default()) && seen.contains(&key);
        seen.insert(key);
        !replay
    }).cloned().collect()
}

/// A step's state from its last result, whether it was redone and whether it took repairs.
fn label(last: Option<&Value>, redone: bool, repaired: bool) -> &'static str {
    let Some(last) = last else { return "ahead" };
    match last["kind"].as_str().unwrap_or_default() {
        "skipped" => "skipped",
        "task-force" | "unmet" => "failed",
        _ if last["met"] == false => "failed",
        _ if redone => "again",
        _ if repaired => "repaired",
        _ => "done",
    }
}

/// How many steps the run has passed: those before the running step (later ones are still ahead, also after a
/// re-entry, where they hold their earlier results); all once the run has ended; up to the last result of a
/// stopped run.
fn passed_until(keys: &[&str], running: Option<&str>, recorded: &BTreeMap<String, Vec<Value>>, ended: bool) -> usize {
    if let Some(i) = running.and_then(|r| keys.iter().position(|k| *k == r)) {
        return i;
    }
    if ended {
        return keys.len();
    }
    keys.iter().rposition(|k| recorded.contains_key(*k)).map_or(0, |i| i + 1)
}

/// A step's state on the map: running, or for one without a result, switched off, passed or ahead.
fn placed(state: &str, running: bool, switched: bool, passed: bool) -> &'static str {
    match state {
        _ if running => "running",
        "ahead" if switched => "off",
        "ahead" if passed => "notrun",
        "ahead" => "ahead",
        "done" => "done",
        "again" => "again",
        "repaired" => "repaired",
        "skipped" => "skipped",
        _ => "failed",
    }
}

/// One row per step result with its own repair lines folded in: its minutes from its first entry after the result
/// before it, and the minutes paused before that (a halted run's failed attempt, the fix and the restart are a
/// pause between launches, not the next step's time).
pub fn steps(rows: &[Value]) -> Vec<Value> {
    let at = |r: &Value| r["at"].as_f64().unwrap_or(0.0);
    let (mut out, mut repairs, mut begun): (Vec<Value>, BTreeMap<String, u64>, BTreeMap<String, f64>) = (vec![], BTreeMap::new(), BTreeMap::new());
    let mut last = rows.first().map_or(0.0, at);
    for r in rows {
        let step = r["step"].as_str().unwrap_or_default().to_string();
        begun.entry(step.clone()).or_insert_with(|| at(r));
        if r["kind"] == "repair" {
            *repairs.entry(step.clone()).or_default() += 1;
        }
        if NOT_RESULTS.contains(&r["kind"].as_str().unwrap_or_default()) {
            continue;
        }
        let first = begun.get(&step).copied().unwrap_or(at(r));
        let begin = if first < at(r) { last.max(first) } else { last };
        let mut row = r.clone();
        row["minutes"] = json!((at(r) - begin) / 60.0);
        row["paused"] = json!((begin - last) / 60.0);
        row["repairs"] = json!(repairs.remove(&step).unwrap_or(0));
        out.push(row);
        last = at(r);
        begun.clear();
    }
    out
}

/// Whether a checkpoint row is a human's answer (not one kept because its inputs were unchanged).
fn asked(row: &Value) -> bool {
    matches!(row["kind"].as_str(), Some("checkpoint" | "approval")) && !row["rationale"].as_str().unwrap_or_default().starts_with("unchanged since")
}

/// The text form of a report for a terminal.
fn text(kind: &str, report: &Value) -> String {
    match kind {
        "map" => report["phases"].as_array().into_iter().flatten().map(|p| {
            let steps: Vec<String> = p["steps"].as_array().into_iter().flatten()
                .map(|s| format!("{}:{}", s["n"].as_str().unwrap_or_default(), s["state"].as_str().unwrap_or_default())).collect();
            format!("{} {}  {}", p["n"], p["name"].as_str().unwrap_or_default(), steps.join(" "))
        }).collect::<Vec<_>>().join("\n"),
        "timeline" => {
            let mut out: Vec<String> = report["steps"].as_array().into_iter().flatten().map(|r| format!(
                "{:6.1} min  {:<10} {}{}{}", r["minutes"].as_f64().unwrap_or(0.0), r["kind"].as_str().unwrap_or_default(),
                if r["met"] == false { "NOT MET " } else { "" }, r["step"].as_str().unwrap_or_default(),
                match r["repairs"].as_u64().unwrap_or(0) { 0 => String::new(), n => format!("  ({n} repairs)") })).collect();
            let (wall, waiting, paused) = (report["wall"].as_f64().unwrap_or(0.0), report["waiting"].as_f64().unwrap_or(0.0), report["paused"].as_f64().unwrap_or(0.0));
            let between = if paused >= 0.1 { format!(", between launches (failed attempts, fixes, restarts) {paused:.1} min") } else { String::new() };
            out.push(String::new());
            out.push(format!("wall {wall:.1} min, of it waiting for humans {waiting:.1} min{between}, the rest {:.1} min", wall - waiting - paused));
            out.push(format!("repairs {}, task force {}, checkpoints asked {}", report["repairs"], report["task_force"], report["checkpoints_asked"]));
            out.join("\n")
        }
        _ => super::profile::render(report),
    }
}

/// `agg process <file> <project> --report map|timeline|profile [--json]`.
pub fn command(file: &Path, project: &Path, args: &[String]) -> Result<ExitCode> {
    let kind = args.iter().position(|a| a == "--report").and_then(|i| args.get(i + 1)).map(String::as_str).unwrap_or_default();
    if !["map", "timeline", "profile"].contains(&kind) {
        bail!("agg process <file> <project> --report map|timeline|profile [--json] [--since <epoch seconds>] [--current <step>] [--stopped] [--log <driver log>]");
    }
    let mut run = Run::open(file, project)?;
    if let Some(since) = args.iter().position(|a| a == "--since").and_then(|i| args.get(i + 1)) {
        // only what the record holds from then on: a smoke run on a copy of an archived project times just its own steps
        let since: f64 = since.parse().map_err(|_| anyhow!("--since takes epoch seconds"))?;
        run.record.retain(|e| e["at"].as_f64().unwrap_or(0.0) >= since);
    }
    // a caller that knows better than state.json (a console that waits at a checkpoint, a stopped run)
    if let Some(current) = args.iter().position(|a| a == "--current").and_then(|i| args.get(i + 1)) {
        (run.current, run.ended) = (Some(current.clone()), false);
    }
    if args.iter().any(|a| a == "--stopped") {
        (run.current, run.ended) = (None, false);
    }
    let report = match kind {
        "map" => run.map(),
        "timeline" => run.timeline(),
        _ => super::profile::profile_with(&run, args.iter().position(|a| a == "--log").and_then(|i| args.get(i + 1)).map(Path::new)),
    };
    if args.iter().any(|a| a == "--json") {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("{}", text(kind, &report));
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(step: &str, kind: &str, met: bool, why: &str, at: u64) -> Value {
        json!({"step": step, "id": step.split(' ').next().unwrap(), "kind": kind, "met": met, "rationale": why, "at": at})
    }

    /// A step's state: done, repaired after a repair, again after a second result or a re-entry, failed, skipped.
    #[test]
    fn a_steps_state_comes_from_its_last_result_and_what_led_to_it() {
        let state = |es: &[Value]| state_of(es)["state"].as_str().unwrap().to_string();
        assert_eq!(state(&[]), "ahead");
        assert_eq!(state(&[e("a", "done", true, "", 1)]), "done");
        assert_eq!(state(&[e("a", "repair", true, "", 1), e("a", "done", true, "", 2)]), "repaired");
        assert_eq!(state(&[e("a", "done", true, "x", 1), e("a", "done", true, "y", 2)]), "again");
        assert_eq!(state(&[e("a", "reentry", true, "", 1), e("a", "done", true, "", 2)]), "again");
        assert_eq!(state(&[e("a", "done", false, "", 1)]), "failed");
        assert_eq!(state(&[e("a", "task-force", true, "re-enter at b: x", 1)]), "failed");
        assert_eq!(state(&[e("a", "skipped", true, "", 1)]), "skipped");
    }

    /// A resumed run's replay of a step is not a second run.
    #[test]
    fn a_resumed_runs_replay_is_not_a_second_run() {
        let once = e("a", "done", true, "same", 1);
        assert_eq!(state_of(&[once.clone(), once])["state"], "done");
    }

    /// A checkpoint's answer: the last of yes, no and auto.
    #[test]
    fn a_checkpoints_answer_is_the_last_one() {
        assert_eq!(state_of(&[e("c", "approval", true, "", 1)])["answer"], "yes");
        assert_eq!(state_of(&[e("c", "approval", false, "", 1)])["answer"], "no");
        assert_eq!(state_of(&[e("c", "auto-approval", true, "", 1)])["answer"], "auto");
        assert_eq!(state_of(&[e("c", "done", true, "", 1)])["answer"], Value::Null);
    }

    /// A step without a result is running, off, passed or ahead.
    #[test]
    fn a_step_without_a_result_is_running_off_passed_or_ahead() {
        assert_eq!(placed("ahead", true, false, false), "running");
        assert_eq!(placed("ahead", false, true, true), "off");
        assert_eq!(placed("ahead", false, false, true), "notrun");
        assert_eq!(placed("ahead", false, false, false), "ahead");
        assert_eq!(placed("done", false, true, false), "done");
    }

    /// How far a run has come: up to the running step, all when ended, up to the last result when stopped.
    #[test]
    fn how_far_a_run_has_come() {
        let keys = ["a", "b", "c", "d"];
        let mut recorded = BTreeMap::new();
        recorded.insert("b".to_string(), vec![]);
        assert_eq!(passed_until(&keys, Some("c"), &recorded, false), 2);
        assert_eq!(passed_until(&keys, None, &recorded, true), 4);
        assert_eq!(passed_until(&keys, None, &recorded, false), 2);
        assert_eq!(passed_until(&keys, None, &BTreeMap::new(), false), 0);
    }

    /// The timeline: a step's minutes run from the result before it; a restart's gap is paused time.
    #[test]
    fn the_timeline_counts_minutes_from_the_result_before_and_pauses_between_launches() {
        let rows = vec![e("a", "done", true, "", 0), e("b", "hook", true, "", 600), e("b", "repair", false, "x", 900),
                        e("b", "done", true, "", 1200), e("c", "approval", true, "", 1260)];
        let table = steps(&rows);
        assert_eq!(table.len(), 3);
        assert_eq!(table[1]["repairs"], 1);
        assert_eq!(table[1]["minutes"].as_f64().unwrap(), 10.0, "from its first entry (a hook at 600) to its result");
        assert_eq!(table[1]["paused"].as_f64().unwrap(), 10.0, "the 10 minutes before its first entry");
        assert!(asked(&table[2]) && !asked(&e("c", "approval", true, "unchanged since x", 1)));
    }
}
