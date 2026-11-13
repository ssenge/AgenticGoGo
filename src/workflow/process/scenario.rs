//! Scenario mode (A-38): `agg process <file> --scenario <scenario.yaml>…` plays a process on a copy
//! of an archived project with scripted agents (`agg scripted-agent`, installed as `claude` and `codex`) and scripted
//! answers to its asks, then checks what the run record and the agents' calls show. No tokens, seconds to minutes.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use super::format::Process;
use super::number::{number, Numbered};

/// One rule of a scripted agent: what it does when a call matches.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// a regex over the prompt and the brief file it names
    pub when: String,
    /// `worker` (the default) or `judge`
    pub kind: Option<String>,
    /// `claude` or `codex`: only calls of that agent
    pub agent: Option<String>,
    /// how often the rule answers
    pub times: Option<usize>,
    /// files written first: path → text
    #[serde(default)]
    pub write: BTreeMap<String, String>,
    /// a shell command run next, the prompt in the file `$AGG_SCENARIO_PROMPT` names
    pub run: Option<String>,
    /// text written into the answer file the brief names (`Write it to <file> as YAML`)
    pub answer: Option<String>,
    /// what the agent replies
    pub say: Option<String>,
}

/// An answer to one of the run's asks.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    /// a regex over the question
    pub ask: String,
    /// default `yes`
    pub say: Option<String>,
    /// appended to the decisions file the question names
    pub decisions: Option<String>,
}

/// What the run must show.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// regexes over `<step key> <kind> <met|unmet>: <why>`, in order
    #[serde(default)]
    pub record: Vec<String>,
    /// regexes over the agents' calls (`<agent> <kind>: <title>`), in order
    #[serde(default)]
    pub sessions: Vec<String>,
    /// regexes no record line may match
    #[serde(default)]
    pub never: Vec<String>,
    /// the project's file after the run matches the regex
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    pub exit: Option<i32>,
}

/// A scenario file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// a path in the projects' folder (or absolute): copied with its siblings
    pub project: String,
    /// shell commands run in the copy and committed as "scenario setup"; `{label:<id>}` is that step's label
    #[serde(default)]
    pub setup: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// several runs, each a list of arguments (instead of `args`)
    pub runs: Option<Vec<Vec<String>>>,
    pub minutes: Option<u64>,
    #[serde(default)]
    pub agents: Vec<Rule>,
    #[serde(default)]
    pub answers: Vec<Answer>,
    #[serde(default)]
    pub expect: Expect,
}

impl Scenario {
    /// The scenario in `path`.
    pub fn read(path: &Path) -> Result<Scenario, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// `agg process <file> --scenario <scenario>… [--projects <dir>] [--keep]`: plays each scenario; exit 0 only when all are
/// as expected.
pub fn play(file: &Path, scenarios: &[PathBuf], projects: Option<&Path>, keep: bool) -> Result<ExitCode> {
    let text = std::fs::read_to_string(file).map_err(|e| anyhow!("{}: {e}", file.display()))?;
    let spec = Process::parse(&text).map_err(|e| anyhow!("{}: {e}", file.display()))?;
    let list = number(&spec);
    let projects = projects.map(Path::to_path_buf).unwrap_or_else(|| scenarios.first().and_then(|s| s.parent()).unwrap_or(Path::new(".")).to_path_buf());
    let mut wrong = vec![];
    for path in scenarios {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("scenario").to_string();
        let found = played(file, &spec, &list, path, &projects, keep).with_context(|| format!("scenario {name}"))?;
        match found.first() {
            None => println!("PASS {name}"),
            Some(first) => println!("FAIL {name}: {first}"),
        }
        wrong.extend(found.into_iter().map(|w| format!("{name}: {w}")));
    }
    let met = wrong.is_empty();
    let rationale = if met { format!("{} as expected", scenarios.len()) } else { wrong.join("; ") };
    println!("{}", serde_json::json!({"met": met, "rationale": rationale}));
    Ok(if met { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// Plays one scenario in a temporary folder, which stays for a look when the scenario shows what it didn't expect (or
/// with `keep`); what it didn't expect, if anything.
fn played(file: &Path, spec: &Process, list: &[Numbered], path: &Path, projects: &Path, keep: bool) -> Result<Vec<String>> {
    let scenario = Scenario::read(path).map_err(|e| anyhow!(e))?;
    let work = std::env::temp_dir().join(format!("agg-scenario-{}-{}", std::process::id(), path.file_stem().and_then(|s| s.to_str()).unwrap_or("x")));
    let _ = removed(&work);
    std::fs::create_dir_all(&work)?;
    let (project, bin) = prepared(&scenario, list, projects, &work)?;
    let history = spec.run.history.clone().unwrap_or_else(|| "agg/private/steps.jsonl".into());
    let (calls, before) = (work.join("calls.jsonl"), lines(&project.join(&history)).len());
    let scenario_file = std::fs::canonicalize(path)?;
    let path_var = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let env = [("PATH", path_var), ("AGG_SCENARIO", scenario_file.display().to_string()), ("AGG_SCENARIO_LOG", calls.display().to_string())];
    let start = Instant::now();
    let (code, problems) = ran(file, &project, &scenario, &env, &work.join("run.log"))?;
    let record: Vec<String> = lines(&project.join(&history)).iter().skip(before).map(record_line).collect();
    let sessions: Vec<String> = lines(&calls).iter().map(|c| format!("{} {}: {}", c["agent"].as_str().unwrap_or_default(), c["kind"].as_str().unwrap_or_default(), c["title"].as_str().unwrap_or_default())).collect();
    let mut wrong = problems;
    wrong.extend(judged(&scenario.expect, &record, &sessions, code)?);
    wrong.extend(kept(&scenario.expect, &project)?);
    if !wrong.is_empty() || keep {
        println!("== {}: exit {code}, {:.0} s, in {}", path.file_stem().and_then(|s| s.to_str()).unwrap_or_default(), start.elapsed().as_secs_f64(), work.display());
        for (head, items) in [("record:", &record), ("agents:", &sessions), ("unexpected:", &wrong)] {
            if !items.is_empty() {
                println!("{head}");
                items.iter().for_each(|i| println!("{i}"));
            }
        }
    }
    if wrong.is_empty() && !keep {
        let _ = removed(&work);
    }
    Ok(wrong)
}

/// The project copied into `work` with its sibling folders, its `setup` run and committed, and a folder of scripted agents.
fn prepared(scenario: &Scenario, list: &[Numbered], projects: &Path, work: &Path) -> Result<(PathBuf, PathBuf)> {
    let named = scenario.project.strip_prefix("~/").map(|rest| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest)).unwrap_or_else(|| PathBuf::from(&scenario.project));
    let source = projects.join(named);
    let parent = source.parent().ok_or_else(|| anyhow!("{}: no folder", source.display()))?;
    copy_tree(parent, &work.join("projects"))?;
    let project = work.join("projects").join(source.file_name().ok_or_else(|| anyhow!("{}: no name", source.display()))?);
    for command in &scenario.setup {
        let status = Command::new("sh").arg("-c").arg(labelled(command, list)).current_dir(&project).status()?;
        if !status.success() {
            return Err(anyhow!("setup `{command}` failed"));
        }
    }
    if !scenario.setup.is_empty() {
        Command::new("git").args(["commit", "-qam", "scenario setup"]).current_dir(&project).status()?;
    }
    let bin = work.join("bin");
    std::fs::create_dir_all(&bin)?;
    let exe = std::env::current_exe()?;
    for agent in ["claude", "codex"] {
        let wrapper = bin.join(agent);
        std::fs::write(&wrapper, format!("#!/bin/sh\nexec \"{}\" scripted-agent {agent} \"$@\"\n", exe.display()))?;
        std::fs::set_permissions(&wrapper, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755))?;
    }
    Ok((project, bin))
}

/// The command with each `{label:<id>}` replaced by that step's label (as an open point carries it).
fn labelled(command: &str, list: &[Numbered]) -> String {
    let re = Regex::new(r"\{label:([\w-]+)\}").expect("a fixed pattern");
    re.replace_all(command, |c: &regex::Captures| {
        list.iter().find(|n| n.id == c[1]).and_then(Numbered::label).unwrap_or_else(|| c[0].to_string())
    })
    .into_owned()
}

/// The JSON lines of a file, none if it doesn't exist.
fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap_or_default().lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The asks no one answered yet.
fn open_asks(project: &Path) -> Vec<Value> {
    let asks = lines(&project.join("agg/private/asks.jsonl"));
    let answered: Vec<&str> = asks.iter().filter(|a| a["state"] == "answered").filter_map(|a| a["id"].as_str()).collect();
    asks.iter().filter(|a| a["state"] == "open" && !answered.contains(&a["id"].as_str().unwrap_or_default())).cloned().collect()
}

/// Answers `ask` as the first unused answer whose `ask` (a regex) matches it says: its `decisions` go into the file the
/// question names first. None when no answer matches.
fn reply(project: &Path, question: &str, answers: &[Answer], used: &mut Vec<usize>) -> Result<Option<String>> {
    let target = Regex::new(r"into (\S+)\.$")?;
    for (i, answer) in answers.iter().enumerate() {
        if used.contains(&i) || !Regex::new(&answer.ask)?.is_match(question) {
            continue;
        }
        used.push(i);
        let named = target.captures(question.trim()).map(|c| c[1].to_string());
        if let (Some(decisions), Some(file)) = (&answer.decisions, named) {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(project.join(file))?;
            writeln!(f, "\n{}", decisions.trim())?;
        }
        return Ok(Some(answer.say.clone().unwrap_or_else(|| "yes".into())));
    }
    Ok(None)
}

/// Answers every new open ask; the asks no answer foresaw are answered no and returned.
fn answered(project: &Path, answers: &[Answer], used: &mut Vec<usize>, seen: &mut Vec<String>) -> Result<Vec<String>> {
    let mut unforeseen = vec![];
    for ask in open_asks(project) {
        let id = ask["id"].as_str().unwrap_or_default().to_string();
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        let question = ask["question"].as_str().unwrap_or_default();
        let said = reply(project, question, answers, used)?;
        if said.is_none() {
            unforeseen.push(format!("unforeseen ask: {}", question.chars().take(300).collect::<String>()));
        }
        let exe = std::env::current_exe()?;
        let _ = Command::new(exe).args(["answer", "--dir"]).arg(project).arg(&id).arg(said.as_deref().unwrap_or("no")).output();
    }
    Ok(unforeseen)
}

/// Runs the process once per run (`runs`: a list of argument lists, or `args`: one), each to its end, answering its asks;
/// the last run's exit code and what went wrong around them.
fn ran(file: &Path, project: &Path, scenario: &Scenario, env: &[(&str, String)], log: &Path) -> Result<(i32, Vec<String>)> {
    let minutes = scenario.minutes.unwrap_or(20);
    let deadline = Instant::now() + Duration::from_secs(60 * minutes);
    let (mut problems, mut used, mut code) = (vec![], vec![], 0);
    let runs = scenario.runs.clone().unwrap_or_else(|| vec![scenario.args.clone()]);
    for args in runs {
        let out = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("process").arg(file).arg(project).args(&args).envs(env.iter().map(|(k, v)| (*k, v))).process_group(0)
            .stdout(Stdio::from(out.try_clone()?)).stderr(Stdio::from(out));
        let mut driver = command.spawn()?;
        let mut seen: Vec<String> = open_asks(project).iter().filter_map(|a| a["id"].as_str().map(String::from)).collect();
        let status = loop {
            if let Some(s) = driver.try_wait()? {
                break s;
            }
            problems.extend(answered(project, &scenario.answers, &mut used, &mut seen)?);
            if Instant::now() > deadline {
                let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(driver.id() as i32), nix::sys::signal::Signal::SIGKILL);
                problems.push(format!("timed out after {minutes} minutes"));
                break driver.wait()?;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        code = status.code().unwrap_or(-1);
    }
    Ok((code, problems))
}

/// The first pattern no line after the previous pattern's line matches; None when all match in order.
fn in_order(patterns: &[String], found: &[String]) -> Result<Option<String>> {
    let mut i = 0;
    for pattern in patterns {
        let re = Regex::new(pattern)?;
        while i < found.len() && !re.is_match(&found[i]) {
            i += 1;
        }
        if i == found.len() {
            return Ok(Some(pattern.clone()));
        }
        i += 1;
    }
    Ok(None)
}

/// What the run shows that the scenario doesn't expect: `record` and `sessions` (regexes, in order), `never` (regexes no
/// record line matches) and `exit`.
fn judged(expect: &Expect, record: &[String], sessions: &[String], code: i32) -> Result<Vec<String>> {
    let mut wrong = vec![];
    for (key, patterns, found) in [("record", &expect.record, record), ("sessions", &expect.sessions, sessions)] {
        if let Some(missing) = in_order(patterns, found)? {
            wrong.push(format!("{key}: nothing in order matches {missing:?}"));
        }
    }
    for pattern in &expect.never {
        let re = Regex::new(pattern)?;
        wrong.extend(record.iter().filter(|l| re.is_match(l)).map(|l| format!("never: {l}")));
    }
    if expect.exit.is_some_and(|e| e != code) {
        wrong.push(format!("exit: {code}, expected {}", expect.exit.unwrap_or_default()));
    }
    Ok(wrong)
}

/// The project's files whose text after the run doesn't match the scenario's regex for it (`files`).
fn kept(expect: &Expect, project: &Path) -> Result<Vec<String>> {
    let mut wrong = vec![];
    for (name, pattern) in &expect.files {
        let text = std::fs::read_to_string(project.join(name)).unwrap_or_default();
        if !Regex::new(pattern)?.is_match(&text) {
            wrong.push(format!("files: {name} doesn't match {pattern:?}"));
        }
    }
    Ok(wrong)
}

/// One entry of the run record as a line: step key, kind, met or unmet, rationale.
fn record_line(entry: &Value) -> String {
    let met = if entry["met"] == true { "met" } else { "unmet" };
    let why = entry["rationale"].as_str().unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
    let who = entry["id"].as_str().or(entry["step"].as_str()).unwrap_or_default();
    format!("{who} {} {met}: {why}", entry["kind"].as_str().unwrap_or_default())
}

/// A folder copied with its symlinks kept as they are.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Removes a scenario's folder, the read-only files of its project too.
fn removed(folder: &Path) -> Result<()> {
    fn writable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(path) else { return };
        if meta.file_type().is_symlink() {
            return;
        }
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(meta.permissions().mode() | 0o200));
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                entries.flatten().for_each(|e| writable(&e.path()));
            }
        }
    }
    writable(folder);
    if folder.exists() {
        std::fs::remove_dir_all(folder)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A scenario's keys are held to the format: a typo is an error, the defaults are the Python harness's.
    #[test]
    fn a_scenario_parses_with_its_defaults_and_refuses_unknown_keys() {
        let s: Scenario = serde_yaml::from_str("project: archive/p\nagents:\n  - { when: '^# Step', say: done }\nexpect: { exit: 0 }\n").unwrap();
        assert_eq!((s.minutes, s.args.len(), s.agents[0].kind.as_deref(), s.agents[0].times), (None, 0, None, None));
        assert!(serde_yaml::from_str::<Scenario>("project: p\nagnets: []\n").is_err());
        assert!(serde_yaml::from_str::<Rule>("when: x\nwrit: {}\n").is_err());
    }

    /// Patterns must match lines in order, each after the one before; the first that doesn't is named.
    #[test]
    fn patterns_match_in_order() {
        let found = strings(&["a done", "b tool", "c done", "d tool"]);
        assert_eq!(in_order(&strings(&["^a", "^c", "d tool$"]), &found).unwrap(), None);
        assert_eq!(in_order(&strings(&["^c", "^a"]), &found).unwrap().as_deref(), Some("^a"), "out of order");
        assert_eq!(in_order(&strings(&["^z"]), &found).unwrap().as_deref(), Some("^z"));
        assert_eq!(in_order(&[], &found).unwrap(), None);
        assert!(in_order(&strings(&["("]), &found).is_err(), "a bad regex is an error, not a silent pass");
    }

    /// Everything a scenario can expect: record and sessions in order, `never`, `exit`, and the files.
    #[test]
    fn what_a_run_shows_is_held_to_what_the_scenario_expects() {
        let expect: Expect = serde_yaml::from_str("record: ['^build done']\nsessions: ['claude worker']\nnever: [' task-force ']\nexit: 3\nfiles: { a.txt: 'x+' }\n").unwrap();
        let (record, sessions) = (strings(&["build done met: fine", "ship task-force unmet: no"]), strings(&["claude worker: Step build"]));
        let wrong = judged(&expect, &record, &sessions, 0).unwrap();
        assert_eq!(wrong, ["never: ship task-force unmet: no", "exit: 0, expected 3"]);
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(kept(&expect, tmp.path()).unwrap(), ["files: a.txt doesn't match \"x+\""]);
        std::fs::write(tmp.path().join("a.txt"), "xx").unwrap();
        assert!(kept(&expect, tmp.path()).unwrap().is_empty());
    }

    /// A record line is the step's key (else its label), the kind, met or unmet, and the rationale on one line.
    #[test]
    fn a_record_line_names_the_key_the_kind_and_the_verdict() {
        let entry = serde_json::json!({"step": "1.3 Survey", "id": "survey", "kind": "done", "met": true, "rationale": "fine\n  and\tdone"});
        assert_eq!(record_line(&entry), "survey done met: fine and done");
        assert_eq!(record_line(&serde_json::json!({"step": "start", "kind": "start", "met": false})), "start start unmet: ");
    }

    /// An answer is the first unused one whose regex matches; its decisions go into the file the question names.
    #[test]
    fn an_ask_is_answered_by_the_first_unused_answer_that_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let answers: Vec<Answer> = serde_yaml::from_str("- { ask: 'cp-a', say: 'yes', decisions: '- change it' }\n- { ask: 'cp-a' }\n- { ask: 'other', say: 'no' }\n").unwrap();
        let mut used = vec![];
        let q = "cp-a: approve? Write what to change into app/decisions.md.";
        assert!(reply(tmp.path(), q, &answers, &mut used).is_err(), "the decisions go into a folder that must exist");
        used.clear();
        std::fs::create_dir_all(tmp.path().join("app")).unwrap();
        assert_eq!(reply(tmp.path(), q, &answers, &mut used).unwrap().as_deref(), Some("yes"));
        assert_eq!(std::fs::read_to_string(tmp.path().join("app/decisions.md")).unwrap(), "\n- change it\n");
        assert_eq!(reply(tmp.path(), q, &answers, &mut used).unwrap().as_deref(), Some("yes"), "the second, unused one");
        assert_eq!(reply(tmp.path(), q, &answers, &mut used).unwrap(), None, "no answer left for it");
        assert_eq!(reply(tmp.path(), "other?", &answers, &mut used).unwrap().as_deref(), Some("no"));
    }
}
