//! The process runs' part of `agg serve` (A-39): which project a request is about (`?p=` in a projects folder), start,
//! resume and stop a run, make a new project from the process file's form, the checkpoints waiting for a human with
//! their documents, the decisions written with an answer, and the process's own facts merged into the run report.
//!
//! Each action is what a human does in the terminal (`agg process <file> <project> [--resume]`, SIGTERM to the run,
//! the decisions file, `agg answer`), so the page and the terminal can be used side by side.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::workflow::process::checkpoint::decisions_file;
use crate::workflow::process::exec::absolutize_in;
use crate::workflow::process::format::{Kind, Process, Step, UiSpec};
use crate::workflow::process::{number::number, template};

/// A refusal: its HTTP status and why.
pub type Refused = (u16, String);

/// Where the server's projects are: one project (`--dir`), or a folder of them (`--projects`, a request names one with
/// `?p=`); and the process file a new run is started with (`--process`), when the project has run none yet.
pub struct Site {
    pub dir: PathBuf,
    pub projects: Option<PathBuf>,
    pub process: Option<PathBuf>,
}

/// The value of query parameter `key` in `url`, percent-decoded.
pub fn query(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')).map(decode)
}

/// `%xx` and `+` decoded.
fn decode(s: &str) -> String {
    let (bytes, mut out, mut i) = (s.as_bytes(), Vec::new(), 0);
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A plain name: lowercase letters, digits and `-`, starting with a letter or digit, at most 41 characters.
pub fn plain_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 41
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Site {
    /// The project a request is about: `?p=<name>` in the projects folder, else the server's own; refused when the name
    /// is no folder of the projects folder.
    pub fn project(&self, url: &str) -> Result<PathBuf, Refused> {
        let (Some(root), Some(name)) = (&self.projects, query(url, "p").filter(|n| !n.is_empty())) else {
            return Ok(self.dir.clone());
        };
        let dir = root.join(&name);
        if name.contains('/') || name.starts_with('.') || !dir.is_dir() {
            return Err((404, format!("no project {name:?} in {}", root.display())));
        }
        Ok(dir)
    }

    /// The process file of `project`: the one it last ran, else the server's.
    pub fn process_of(&self, project: &Path) -> Option<PathBuf> {
        let note = std::fs::read_to_string(crate::paths::private_dir(project).join("process.json")).ok();
        let ran = note.and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["file"].as_str().map(PathBuf::from));
        ran.or_else(|| self.process.clone())
    }

    /// The projects of the projects folder (those that ran a process), the latest first: whether each runs, where it
    /// is, how many asks wait.
    pub fn projects(&self) -> Value {
        let Some(root) = &self.projects else { return json!({"projects": [], "folder": null}) };
        let mut found: Vec<(std::time::SystemTime, Value)> = std::fs::read_dir(root).into_iter().flatten().flatten()
            .map(|e| e.path())
            .filter(|d| crate::paths::private_dir(d).join("process.json").exists())
            .map(|d| (changed(&d), summary(&d)))
            .collect();
        found.sort_by_key(|f| std::cmp::Reverse(f.0));
        json!({"projects": found.into_iter().map(|(_, v)| v).collect::<Vec<_>>(), "folder": root})
    }

    /// The process's UI section: the new-project form (only where a projects folder takes new projects).
    pub fn ui(&self) -> Value {
        let ui = self.process.as_deref().and_then(|f| spec(f).ok()).and_then(|p| p.ui).unwrap_or_default();
        let form = ui.new_project.filter(|_| self.projects.is_some()).map(|n| json!({"fields": n.fields}));
        json!({"new_project": form, "overlay": ui.overlay.is_some()})
    }
}

/// When a project last changed: its state, else its process note.
fn changed(project: &Path) -> std::time::SystemTime {
    let private = crate::paths::private_dir(project);
    ["state.json", "process.json"].iter().filter_map(|f| std::fs::metadata(private.join(f)).ok()?.modified().ok())
        .max().unwrap_or(std::time::UNIX_EPOCH)
}

/// A project's row: its name, whether a run is alive, the step its state names, its open asks.
fn summary(project: &Path) -> Value {
    let state: Value = std::fs::read_to_string(crate::paths::private_dir(project).join("state.json")).ok()
        .and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let running = crate::os::detach::live_pid(project).is_some();
    json!({"name": project.file_name().and_then(|n| n.to_str()).unwrap_or_default(), "running": running,
           "step": state["step"], "asks": if running { crate::core::asks::open(project).len() } else { 0 }})
}

/// The process file, parsed.
fn spec(file: &Path) -> anyhow::Result<Process> {
    Process::parse(&std::fs::read_to_string(file).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?)
}

/// `start` (a fresh run), `resume` or `stop` the run of `project` under `process`.
pub fn act(project: &Path, process: Option<&Path>, action: &str) -> Result<Value, Refused> {
    let pid = crate::os::detach::live_pid(project);
    match (action, pid) {
        ("stop", Some(pid)) => {
            // SIGTERM: the run interrupts its session and ends; `resume` continues where it stopped
            crate::os::proc::term_pid(pid).then(|| json!({"ok": true})).ok_or((500, format!("could not signal {pid}")))
        }
        ("stop", None) => Err((409, "no run on this project".into())),
        ("start" | "resume", Some(pid)) => Err((409, format!("a run is alive here (pid {pid})"))),
        ("start" | "resume", None) => {
            let file = process.ok_or((409, "no process file: the project ran none, and the server has no --process".to_string()))?;
            let mut args = vec!["process".to_string(), file.display().to_string(), project.display().to_string()];
            if action == "resume" {
                args.push("--resume".into());
            }
            let pid = crate::os::detach::spawn_logged(project, &args).map_err(|e| (500, e.to_string()))?;
            Ok(json!({"ok": true, "pid": pid}))
        }
        (other, _) => Err((400, format!("no action {other:?} (start, resume, stop)"))),
    }
}

/// Makes the project `body.name` in `root` with the process's new-project command (each field in its environment), then
/// starts its run.
pub fn create(root: &Path, file: &Path, body: &Value) -> Result<Value, Refused> {
    let ui = spec(file).map_err(|e| (500, e.to_string()))?.ui.unwrap_or_default();
    let form = ui.new_project.ok_or((404, "the process declares no new_project form".to_string()))?;
    let name = body["name"].as_str().unwrap_or_default().trim();
    if !plain_name(name) {
        return Err((400, "the name: lowercase letters, digits and -, e.g. orders-2026".into()));
    }
    if root.join(name).exists() {
        return Err((409, format!("{name} exists already")));
    }
    let mut cmd = Command::new("sh");
    let home = file.parent().unwrap_or(Path::new("."));
    cmd.arg("-c").arg(format!("{} \"$0\"", absolutize_in(home, &form.command))).arg(root.join(name)).current_dir(root);
    for f in &form.fields {
        let value = body[&f.name].as_str().unwrap_or_default().trim().to_string();
        if f.required && value.is_empty() {
            return Err((400, format!("{} is needed", f.label)));
        }
        cmd.env(format!("AGG_FIELD_{}", f.name.to_uppercase()), value);
    }
    let out = crate::os::proc::run_with_timeout(cmd, 300).map_err(|e| (500, e))?;
    if !out.success || !root.join(name).is_dir() {
        let said = String::from_utf8_lossy(&out.stderr);
        return Err((400, said.trim().lines().last().unwrap_or("the new-project command failed").to_string()));
    }
    act(&root.join(name), Some(file), "start").map(|_| json!({"ok": true, "project": name}))
}

/// The checkpoints waiting for a human while a run is alive: each open ask whose question names a checkpoint's decisions
/// file (agg's approval question does), with the checkpoint's document, decisions so far and the open points.
pub fn waiting(project: &Path, file: Option<&Path>) -> Value {
    if crate::os::detach::live_pid(project).is_none() {
        return json!({"checkpoints": []});
    }
    let points = file.and_then(|f| spec(f).ok()).and_then(|p| p.run.open_points);
    let read = |rel: &str| std::fs::read_to_string(project.join(rel)).unwrap_or_default();
    let found: Vec<Value> = crate::core::asks::open(project).into_iter().filter_map(|a| {
        let (id, doc, decisions) = file.and_then(|f| checkpoint_of(f, project, &a.question))?;
        Some(json!({"ask": a.id, "question": a.question, "step": id, "doc": doc, "document": read(&doc),
                    "decisions_file": decisions, "decisions": read(&decisions),
                    "open_points_file": points, "open_points": points.as_deref().map(read).unwrap_or_default()}))
    }).collect();
    json!({"checkpoints": found})
}

/// The checkpoint whose decisions file `question` names: its id, its document and the decisions file, as the run
/// renders them.
fn checkpoint_of(file: &Path, project: &Path, question: &str) -> Option<(String, String, String)> {
    let process = spec(file).ok()?;
    let list = number(&process);
    let home = file.parent().unwrap_or(Path::new("."));
    let vars = template::variables(&process, &list, home, &project.display().to_string()).ok()?;
    let mut steps: Vec<&Step> = Vec::new();
    for p in &process.phases {
        collect(&p.steps, &mut steps);
    }
    steps.into_iter().filter(|s| s.kind == Kind::Checkpoint).find_map(|s| {
        let doc = template::render(s.doc.as_deref()?, &vars).ok()?;
        let decisions = decisions_file(&s.id, &doc);
        question.contains(&format!("into {decisions}")).then(|| (s.id.clone(), doc, decisions))
    })
}

/// Every step of `steps`, an `each`'s own steps included.
fn collect<'a>(steps: &'a [Step], out: &mut Vec<&'a Step>) {
    for s in steps {
        out.push(s);
        collect(&s.steps, out);
    }
}

/// Appends the humans' decisions to the decisions file of the checkpoint ask `ask` waits at, one list item per line,
/// before the answer is given; refused for an ask that is no checkpoint's.
pub fn decide(project: &Path, file: Option<&Path>, ask: &str, text: &str) -> Result<(), Refused> {
    let lines: Vec<String> = text.lines().filter(|l| !l.trim().is_empty())
        .map(|l| if l.starts_with('-') || l.starts_with('*') { l.to_string() } else { format!("- {l}") }).collect();
    if lines.is_empty() {
        return Ok(());
    }
    let question = crate::core::asks::get(project, ask).map(|a| a.question).ok_or((404, format!("no ask {ask}")))?;
    let (_, _, decisions) = file.and_then(|f| checkpoint_of(f, project, &question))
        .ok_or((400, "decisions go with a checkpoint's ask only".to_string()))?;
    let path = project.join(&decisions);
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    let gap = if before.is_empty() || before.ends_with('\n') { "" } else { "\n" };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| (500, e.to_string()))?;
    }
    std::fs::write(&path, format!("{before}{gap}{}\n", lines.join("\n"))).map_err(|e| (500, e.to_string()))
}

/// The report with the process's own facts (its `ui.overlay`) merged in: each step's into its `extra`, the run's into
/// the report's `extra`; a failing overlay leaves the report as agg made it, with `overlay_error` saying why.
pub fn overlaid(mut report: Value, project: &Path, file: Option<&Path>, kind: &str) -> Value {
    let Some((file, ui)) = file.and_then(|f| Some((f, spec(f).ok()?.ui?))) else { return report };
    let UiSpec { overlay: Some(command), .. } = ui else { return report };
    match overlay(file, &command, project, kind, &report) {
        Ok(facts) => merge(&mut report, &facts),
        Err(e) => report["overlay_error"] = json!(e),
    }
    report
}

/// The overlay command's facts: run in the project with the report on stdin.
fn overlay(file: &Path, command: &str, project: &Path, kind: &str, report: &Value) -> Result<Value, String> {
    let home = file.parent().unwrap_or(Path::new("."));
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(format!("{} \"$0\" \"$1\"", absolutize_in(home, command))).arg(project).arg(kind).current_dir(project);
    let out = crate::os::proc::run_with_input(cmd, report.to_string().as_bytes(), 120)?;
    if !out.success {
        let said = String::from_utf8_lossy(&out.stderr);
        return Err(format!("`{command}` failed: {}", said.trim().lines().last().unwrap_or_default()));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("`{command}` printed no JSON: {e}"))
}

/// `facts.steps[<id>]` into the `extra` of every step with that id (the map's in its phases, the profile's in its
/// steps), `facts.run` into the report's `extra`.
pub fn merge(report: &mut Value, facts: &Value) {
    let steps = facts["steps"].as_object();
    let mut into = |step: &mut Value| {
        let Some(found) = steps.and_then(|s| s.get(step["id"].as_str().unwrap_or_default())) else { return };
        if !step["extra"].is_object() {
            step["extra"] = json!({});
        }
        for (k, v) in found.as_object().into_iter().flatten() {
            step["extra"][k] = v.clone();
        }
    };
    for phase in report["phases"].as_array_mut().into_iter().flatten() {
        phase["steps"].as_array_mut().into_iter().flatten().for_each(&mut into);
    }
    report["steps"].as_array_mut().into_iter().flatten().for_each(&mut into);
    if let Some(map) = report.get_mut("map") {
        for phase in map["phases"].as_array_mut().into_iter().flatten() {
            phase["steps"].as_array_mut().into_iter().flatten().for_each(&mut into);
        }
    }
    if facts["run"].is_object() {
        report["extra"] = facts["run"].clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_value_is_found_and_decoded() {
        assert_eq!(query("/api/process?p=orders-2026&report=map", "p").as_deref(), Some("orders-2026"));
        assert_eq!(query("/x?report=map", "report").as_deref(), Some("map"));
        assert_eq!(query("/x?q=a%20b+c", "q").as_deref(), Some("a b c"));
        assert_eq!(query("/x?pp=1", "p"), None);
        assert_eq!(query("/x", "p"), None);
        assert_eq!(query("/x?q=50%", "q").as_deref(), Some("50%"));
    }

    #[test]
    fn a_plain_name_is_lowercase_letters_digits_and_dashes() {
        assert!(plain_name("orders-2026") && plain_name("a"));
        assert!(!plain_name("") && !plain_name("-a") && !plain_name("Orders") && !plain_name("a/b") && !plain_name("../x"));
        assert!(!plain_name(&"a".repeat(42)));
    }

    #[test]
    fn a_project_is_a_folder_of_the_projects_folder() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("one")).unwrap();
        let site = Site { dir: tmp.path().join("own"), projects: Some(tmp.path().to_path_buf()), process: None };
        assert_eq!(site.project("/api/state?p=one").unwrap(), tmp.path().join("one"));
        assert_eq!(site.project("/api/state").unwrap(), tmp.path().join("own"), "no p: the server's own");
        assert_eq!(site.project("/api/state?p=two").unwrap_err().0, 404);
        assert_eq!(site.project("/api/state?p=..").unwrap_err().0, 404);
        let single = Site { projects: None, ..site };
        assert_eq!(single.project("/api/state?p=one").unwrap(), tmp.path().join("own"), "one project: p is ignored");
    }

    #[test]
    fn the_overlay_facts_go_into_each_steps_extra_and_the_reports() {
        let mut map = json!({"phases": [{"steps": [{"id": "a", "extra": {}}, {"id": "b", "extra": {"x": 1}}]}]});
        merge(&mut map, &json!({"steps": {"b": {"y": "two"}, "c": {"z": 3}}, "run": {"total": 9}}));
        assert_eq!(map["phases"][0]["steps"][0]["extra"], json!({}));
        assert_eq!(map["phases"][0]["steps"][1]["extra"], json!({"x": 1, "y": "two"}));
        assert_eq!(map["extra"], json!({"total": 9}));
        let mut profile = json!({"steps": [{"id": "a"}], "map": {"phases": [{"steps": [{"id": "a", "extra": {}}]}]}});
        merge(&mut profile, &json!({"steps": {"a": {"does": "it"}}}));
        assert_eq!(profile["steps"][0]["extra"]["does"], "it", "a profile step gets an extra");
        assert_eq!(profile["map"]["phases"][0]["steps"][0]["extra"]["does"], "it");
        assert!(profile.get("extra").is_none(), "no run facts, no extra");
    }

    #[test]
    fn decisions_need_a_checkpoints_ask_and_become_list_items() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(decide(tmp.path(), None, "x", " \n").is_ok(), "nothing written: nothing to check");
        assert_eq!(decide(tmp.path(), None, "x", "keep it").unwrap_err().0, 404);
    }
}
