//! A scripted coding agent for scenario mode (A-38): installed as `claude` and as
//! `codex` (hidden subcommand `agg scripted-agent <claude|codex> <the agent's arguments>`), it does what the scenario's
//! first matching rule says, answers as the real agent's output format does, and logs each call. A judge without a rule
//! is met; a worker without one changes nothing.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use fancy_regex::Regex as Pattern;
use regex::Regex;
use serde_json::{json, Value};

use super::scenario::{Rule, Scenario};

/// What agg asks every rubric judge for, last in its prompt.
const VERDICT: &str = "Output ONLY a single JSON object";
const MET: &str = r#"{"met": true, "value": 1, "max": 1, "target": 1, "rationale": "scripted: met"}"#;

/// `agg scripted-agent <agent> <args…>`: answers one call as the agent named first.
pub fn command(args: &[String]) -> ExitCode {
    let (agent, rest) = match args.split_first() {
        Some((a, r)) => (a.as_str(), r),
        None => return ExitCode::from(2),
    };
    if rest.iter().any(|a| a == "--version") {
        println!("{agent} (scripted) 0.0.0");
        return ExitCode::SUCCESS;
    }
    match answer(agent, rest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("scripted agent: {e}");
            ExitCode::from(1)
        }
    }
}

fn answer(agent: &str, args: &[String]) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let (scenario, log) = (env("AGG_SCENARIO")?, PathBuf::from(env("AGG_SCENARIO_LOG")?));
    let rules = Scenario::read(Path::new(&scenario))?.agents;
    let prompt = brief(&prompt_of(agent, args), &cwd);
    let kind = if prompt.contains(VERDICT) { "judge" } else { "worker" };
    let index = rule_for(&rules, &prompt, agent, kind, &log)?;
    let rule = index.map(|i| rules[i].clone()).unwrap_or_default();
    act(&rule, &cwd, &prompt)?;
    let said = rule.say.clone().unwrap_or_else(|| if kind == "judge" { MET.to_string() } else { "done".into() });
    let line = json!({"agent": agent, "kind": kind, "rule": index, "title": title(&prompt)});
    let mut text = std::fs::read_to_string(&log).unwrap_or_default();
    text.push_str(&format!("{line}\n"));
    std::fs::write(&log, text).map_err(|e| format!("{}: {e}", log.display()))?;
    for event in output(agent, args, &said) {
        println!("{event}");
    }
    Ok(())
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}

/// The prompt: `-p <prompt>` for a claude worker, the last argument for a codex one; on stdin for a judge (claude's `-p`
/// with no argument, codex's `-`), since one argument is capped.
fn prompt_of(agent: &str, args: &[String]) -> String {
    let stdin = || {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        s
    };
    if agent == "codex" {
        return match args.last() {
            Some(last) if last == "-" => stdin(),
            Some(last) => last.clone(),
            None => String::new(),
        };
    }
    let Some(at) = args.iter().position(|a| a == "-p") else { return String::new() };
    match args.get(at + 1) {
        Some(next) if !next.starts_with("--") => next.clone(),
        _ => stdin(),
    }
}

/// What the agent is asked: the prompt, and the brief file it names (agg puts a worker's brief there).
fn brief(prompt: &str, cwd: &Path) -> String {
    let named = Regex::new(r"`(agg/private/[^`]+\.md)`").ok().and_then(|re| re.captures(prompt).map(|c| c[1].to_string()));
    match named.and_then(|n| std::fs::read_to_string(cwd.join(n)).ok()) {
        Some(text) => format!("{prompt}\n{text}"),
        None => prompt.to_string(),
    }
}

/// What names the call in the log: the first `# ` heading, else the first line.
fn title(prompt: &str) -> String {
    let heading = Regex::new(r"(?m)^# (.+)$").ok().and_then(|re| re.captures(prompt).map(|c| c[1].to_string()));
    let first = heading.unwrap_or_else(|| prompt.trim().lines().next().unwrap_or_default().to_string());
    first.chars().take(100).collect()
}

/// The calls logged so far.
fn calls(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The first rule matching the prompt (`when`, a regex), the agent (`agent`, if given) and the kind of call (`kind`:
/// `worker`, the default, or `judge`) that has calls left (`times`, unbounded if unset).
fn rule_for(rules: &[Rule], prompt: &str, agent: &str, kind: &str, log: &Path) -> Result<Option<usize>, String> {
    let used: Vec<Option<u64>> = calls(log).iter().map(|c| c["rule"].as_u64()).collect();
    for (i, rule) in rules.iter().enumerate() {
        if rule.agent.as_deref().is_some_and(|a| a != agent) || rule.kind.as_deref().unwrap_or("worker") != kind {
            continue;
        }
        let re = Pattern::new(&format!("(?m){}", rule.when)).map_err(|e| format!("rule {i}: `when`: {e}"))?;
        let times = used.iter().filter(|u| **u == Some(i as u64)).count();
        if super::scenario::hit(&re, prompt) && rule.times.is_none_or(|t| times < t) {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

/// The rule's work in the agent's folder: `write` (path: text), then `run` (a shell command, the prompt in the file
/// `$AGG_SCENARIO_PROMPT` names), then `answer` (text) into the answer file the brief names (`Write it to <file> as YAML`).
fn act(rule: &Rule, cwd: &Path, prompt: &str) -> Result<(), String> {
    for (path, text) in &rule.write {
        let target = cwd.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&target, text).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    if let Some(run) = &rule.run {
        let file = std::env::temp_dir().join(format!("agg-scenario-prompt-{}.md", std::process::id()));
        std::fs::write(&file, prompt).map_err(|e| e.to_string())?;
        let _ = Command::new("sh").arg("-c").arg(run).current_dir(cwd).env("AGG_SCENARIO_PROMPT", &file).status();
        let _ = std::fs::remove_file(&file);
    }
    let named = Regex::new(r"Write it to (\S+) as YAML").ok().and_then(|re| re.captures(prompt).map(|c| c[1].to_string()));
    if let (Some(text), Some(path)) = (&rule.answer, named) {
        std::fs::write(&path, text).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}

/// The events the real agent would print for an answer `said`: codex's `--json`, claude's `stream-json` or its one-shot
/// `json`.
fn output(agent: &str, args: &[String], said: &str) -> Vec<Value> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let sid = format!("scripted-{}-{nanos}", std::process::id());
    let usage = json!({"input_tokens": 1, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0});
    if agent == "codex" {
        return vec![
            json!({"type": "thread.started", "thread_id": sid}),
            json!({"type": "item.completed", "item": {"type": "agent_message", "text": said}}),
            json!({"type": "turn.completed", "usage": usage}),
        ];
    }
    let result = json!({"type": "result", "subtype": "success", "is_error": false, "result": said, "session_id": sid,
        "total_cost_usd": 0.0, "usage": usage});
    if !args.iter().any(|a| a == "stream-json") {
        return vec![result];
    }
    vec![
        json!({"type": "system", "subtype": "init", "session_id": sid, "model": "scripted"}),
        json!({"type": "assistant", "session_id": sid, "message": {"content": [{"type": "text", "text": said}]}}),
        result,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(yaml: &str) -> Vec<Rule> {
        serde_yaml::from_str(yaml).unwrap()
    }

    /// The first rule matching the prompt, the agent and the kind that has calls left answers; `times` counts the calls
    /// the log shows for that rule.
    #[test]
    fn the_first_matching_rule_with_calls_left_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("calls.jsonl");
        let rs = rules("- { when: '^# Step build', times: 1 }\n- { when: 'build', agent: codex }\n- { when: 'verdict', kind: judge }\n- { when: '^# Step' }\n");
        let ask = |prompt: &str, agent: &str, kind: &str| rule_for(&rs, prompt, agent, kind, &log).unwrap();
        assert_eq!(ask("# Step build\n", "claude", "worker"), Some(0));
        std::fs::write(&log, "{\"rule\": 0}\n").unwrap();
        assert_eq!(ask("# Step build\n", "claude", "worker"), Some(3), "rule 0 is used up, rule 1 is codex's");
        assert_eq!(ask("# Step build\n", "codex", "worker"), Some(1));
        assert_eq!(ask("the verdict", "claude", "judge"), Some(2));
        assert_eq!(ask("the verdict", "claude", "worker"), None);
        assert!(rule_for(&rules("- { when: '(' }\n"), "x", "claude", "worker", &log).is_err());
    }

    /// A call is titled by the first `# ` heading, else its first line, cut at 100 characters.
    #[test]
    fn a_call_is_titled_by_its_heading_or_first_line() {
        assert_eq!(title("intro\n# Step 4: Design\nmore"), "Step 4: Design");
        assert_eq!(title("  just a line\nsecond"), "just a line");
        assert_eq!(title(&"x".repeat(150)).len(), 100);
        assert_eq!(title(""), "");
    }

    /// The real agents' output formats: codex's events, claude's one-shot result and its stream.
    #[test]
    fn the_output_follows_the_real_agents_formats() {
        let codex = output("codex", &[], "hi");
        assert_eq!((codex.len(), codex[1]["item"]["text"].as_str()), (3, Some("hi")));
        let one = output("claude", &["-p".into()], "hi");
        assert_eq!((one.len(), one[0]["result"].as_str(), one[0]["type"].as_str()), (1, Some("hi"), Some("result")));
        let stream = output("claude", &["--output-format".into(), "stream-json".into()], "hi");
        assert_eq!(stream.iter().map(|e| e["type"].as_str().unwrap()).collect::<Vec<_>>(), ["system", "assistant", "result"]);
    }

    /// A rule writes its files, runs its command with the prompt in a file, and answers into the file the brief names.
    #[test]
    fn a_rule_writes_runs_and_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let answer = tmp.path().join("answer.yaml");
        let rule = &rules("- { when: x, write: { out/a.txt: hello }, run: 'cp \"$AGG_SCENARIO_PROMPT\" copy.md', answer: 'outcome: fixed' }\n")[0];
        act(rule, tmp.path(), &format!("# Step\nWrite it to {} as YAML\n", answer.display())).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join("out/a.txt")).unwrap(), "hello");
        assert!(std::fs::read_to_string(tmp.path().join("copy.md")).unwrap().contains("# Step"));
        assert_eq!(std::fs::read_to_string(&answer).unwrap(), "outcome: fixed");
    }

    /// The prompt of a worker is the argument after `-p`; the brief file it names is read along.
    #[test]
    fn a_workers_prompt_and_the_brief_it_names() {
        assert_eq!(prompt_of("claude", &["--x".into(), "-p".into(), "the prompt".into()]), "the prompt");
        assert_eq!(prompt_of("codex", &["exec".into(), "the prompt".into()]), "the prompt");
        assert_eq!(prompt_of("claude", &["--x".into()]), "");
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("agg/private")).unwrap();
        std::fs::write(tmp.path().join("agg/private/INSTRUCTIONS.md"), "the brief").unwrap();
        assert_eq!(brief("read `agg/private/INSTRUCTIONS.md`", tmp.path()), "read `agg/private/INSTRUCTIONS.md`\nthe brief");
        assert_eq!(brief("nothing named", tmp.path()), "nothing named");
    }
}
