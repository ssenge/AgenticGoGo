//! The profile of a process run (A-38 3.7): per step where the time went, split into the worker's session, repair
//! sessions, the critic, checks, hooks, the task force and waits for a human; the models and tools a step's sessions
//! used and the files it changed. 

use std::path::Path;

use serde_json::{json, Value};

use super::report::{lines, Run, NOT_RESULTS};

/// Files listed per step; the rest is counted.
const SHOWN: usize = 60;
/// What a step's call is called in a substep.
const ROLE: [(&str, &str); 7] = [("agent", "do"), ("check", "check"), ("critic", "critique"), ("repair", "repair"), ("hooks", "hooks"),
                                 ("force", "task force"), ("human", "human")];
/// A tool step that did nothing in this run says so in its result.
const IDLE: [&str; 5] = ["not built yet", "doesn't apply", "don't apply", "not executed", "no test environment"];

fn num(v: &Value, key: &str) -> f64 {
    v[key].as_f64().unwrap_or(0.0)
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

/// The profile's data: the run's steps with their segments and substeps, and its totals.
pub fn profile(run: &Run) -> Value {
    let p = &run.project;
    let mut calls: Vec<Value> = std::fs::read_dir(p.join("agg/private")).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|f| f.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("calls.") && n.ends_with(".jsonl")))
        .flat_map(|f| lines(&f)).chain(lines(&p.join("agg/private/calls.jsonl"))).collect();
    calls.sort_by_key(|c| c["ts"].as_u64().unwrap_or(0));
    let record = &run.record;
    let start = calls.iter().filter_map(|c| c["ts"].as_f64()).chain(record.iter().filter_map(|r| r["at"].as_f64())).fold(f64::MAX, f64::min);
    let start = if start == f64::MAX { 0.0 } else { start };
    let mut result = results(record, start);
    let spans = spans(&calls);
    assign(&mut result, &spans);
    mark_waits(p, &mut result);
    models(p, &mut result);
    add_files(p, &mut result);
    for s in &mut result {
        s["parts"] = json!(parts(s["segments"].as_array().map(Vec::as_slice).unwrap_or_default()));
    }
    let force = record.iter().filter(|r| r["kind"] == "task-force").count();
    json!({"project": p.file_name().and_then(|n| n.to_str()).unwrap_or_default(), "start": start,
           "end": result.last().map_or(start, |s| num(s, "end")), "steps": result,
           "tokens": spans.iter().map(|s| num(s, "tokens")).sum::<f64>() as u64,
           "cost": (spans.iter().map(|s| num(s, "cost")).sum::<f64>() * 100.0).round() / 100.0,
           "task_force": force, "map": run.map(), "pending": pending(&spans, &result)})
}

/// One entry per step result, in order, each with its start (the result before it), end, the hooks that ran
/// around it and its repairs with their reasons. A step that runs again after the task force sent the run back is
/// "(again)".
fn results(record: &[Value], start: f64) -> Vec<Value> {
    let (mut out, mut hooks, mut repairs, mut last, mut repaired_at): (Vec<Value>, Vec<Value>, Vec<String>, f64, Option<f64>) =
        (vec![], vec![], vec![], start, None);
    let mut ran: Vec<String> = vec![];
    for r in record {
        let kind = text(r, "kind");
        if kind == "task-force" {
            ran = out.iter().map(|s| text(s, "name")).chain([text(r, "step")]).collect();
        }
        if kind == "repair" {
            repairs.push(text(r, "rationale").chars().take(600).collect());
            repaired_at.get_or_insert(num(r, "at"));
        }
        if kind == "hook" {
            hooks.push(hook(r));
        }
        if NOT_RESULTS.contains(&kind.as_str()) {
            continue;
        }
        let name = text(r, "step");
        let again = kind != "task-force" && ran.contains(&name);
        let why = text(r, "rationale");
        let idle = kind == "skipped" || (kind == "tool" && IDLE.iter().any(|i| why.to_lowercase().contains(i)));
        out.push(json!({"name": format!("{name}{}", if again { " (again)" } else { "" }), "id": r["id"], "phase": phase(&name),
                        "kind": kind, "met": r["met"], "start": last, "end": num(r, "at"), "commit": r["commit"],
                        "repairs": repairs.len(), "why_repaired": repairs, "repaired_at": repaired_at, "hooks": hooks,
                        "rationale": why.chars().take(400).collect::<String>(), "segments": [], "inactive": idle,
                        "agent": kind == "done" && !why.starts_with("without a session")}));
        (hooks, repairs, last, repaired_at) = (vec![], vec![], num(r, "at"), None);
    }
    out
}

/// The phase number of a step's label ("6" of "6.3-U1 transform").
fn phase(step: &str) -> String {
    let first = step.split(' ').next().unwrap_or_default().split('.').next().unwrap_or_default();
    if first.chars().all(|c| c.is_ascii_digit()) && !first.is_empty() { first.into() } else { "end".into() }
}

/// A hook's record line ("before declared inputs: …") as when it ran, its name, its verdict and why.
fn hook(entry: &Value) -> Value {
    let why = text(entry, "rationale");
    let (head, rest) = why.split_once(": ").unwrap_or((&why, ""));
    let words: Vec<&str> = head.split(' ').collect();
    let cut = if words.first() == Some(&"phase") { 3 } else { 1 }.min(words.len());
    let name = words[cut..].join(" ");
    json!({"when": words[..cut].join(" "), "name": name.trim_start_matches("hook-").replace('-', " "), "met": entry["met"],
           "why": rest.chars().take(300).collect::<String>()})
}

fn outcome(call: &Value) -> &Value {
    &call["outcome"]
}

/// A call's category and label: the worker's first session, a repair session, the task force, the critic, a check, a
/// hook, a human answer.
fn category(call: &Value, sessions: &mut std::collections::HashMap<String, u32>) -> (&'static str, String) {
    match call["kind"].as_str().unwrap_or_default() {
        "step" => {
            let name = text(outcome(call), "step");
            if name == "task-force" {
                return ("force", "task force session".into());
            }
            let n = sessions.entry(name.clone()).or_default();
            *n += 1;
            if *n == 1 { ("agent", format!("session {name}")) } else { ("repair", format!("repair session {name}")) }
        }
        "judge" => {
            let name = text(call, "judge");
            match name.strip_prefix("hook-") {
                Some(hook) => ("hooks", hook.into()),
                None if num(call, "tokens") > 0.0 || call["critic"].as_bool().unwrap_or(false) => ("critic", name),
                None => ("check", name),
            }
        }
        "hil" => ("human", text(call, "msg").chars().take(80).collect()),
        other => ("driver", other.into()),
    }
}

/// Each call as a span from the call before it to its own end; a session's span is its own seconds, the rest
/// before it is the driver's (worktrees, commits, prompts).
fn spans(calls: &[Value]) -> Vec<Value> {
    let (mut out, mut last, mut sessions) = (vec![], None::<f64>, std::collections::HashMap::new());
    let (mut tokens, mut cost) = (0.0, 0.0);
    for c in calls.iter().filter(|c| c["ts"].is_number()) {
        let ts = num(c, "ts");
        if c["kind"] == "note" {
            last = Some(ts);
            continue;
        }
        let (cat, label) = category(c, &mut sessions);
        let mut begin = last.unwrap_or(ts);
        let mut used = (num(c, "tokens"), num(c, "cost"));
        if c["kind"] == "step" {
            let secs = num(outcome(c), "secs");
            if ts - secs > begin {
                out.push(json!({"cat": "driver", "label": "driver", "start": begin, "end": ts - secs}));
            }
            begin = begin.max(ts - secs);
            // agg reports the run's running totals: a session's share is the growth since the one before
            let own = |now: f64, running: &mut f64| { let d = if now >= *running { now - *running } else { now }; *running = now; d };
            used = (own(num(outcome(c), "tokens"), &mut tokens), own(num(outcome(c), "cost"), &mut cost));
        }
        let verdict = &c["verdict"];
        out.push(json!({"cat": cat, "label": label, "start": begin, "end": ts, "tokens": used.0, "cost": used.1,
                        "session": outcome(c)["session"], "met": verdict["met"],
                        "why": text(verdict, "rationale").chars().take(300).collect::<String>()}));
        last = Some(ts);
    }
    out
}

/// Puts each call span into the step whose time it ends in, clipped to it; time no call covers is the driver's.
fn assign(result: &mut [Value], spans: &[Value]) {
    for span in spans {
        let Some(step) = result.iter_mut().find(|s| num(s, "end") >= num(span, "end")) else { continue };
        let (lo, hi) = (num(span, "start").max(num(step, "start")), num(span, "end").min(num(step, "end")));
        if hi > lo {
            let mut clipped = span.clone();
            clipped["start"] = json!(lo);
            clipped["end"] = json!(hi);
            if let Some(s) = step["segments"].as_array_mut() {
                s.push(clipped);
            }
        }
    }
    for step in result.iter_mut() {
        let covered: f64 = step["segments"].as_array().into_iter().flatten().map(|s| num(s, "end") - num(s, "start")).sum();
        let rest = num(step, "end") - num(step, "start") - covered;
        if rest > 1.0 {
            let end = num(step, "end");
            if let Some(s) = step["segments"].as_array_mut() {
                s.push(json!({"cat": "driver", "label": "not in agg's call log", "start": end - rest, "end": end}));
            }
        }
    }
}

/// The run's waits for a human answer (an ask opened to answered, or to now while open).
fn waits(project: &Path) -> Vec<(f64, f64, String)> {
    let mut open: Vec<Value> = vec![];
    let mut out = vec![];
    for a in lines(&project.join("agg/private/asks.jsonl")) {
        match a["state"].as_str() {
            Some("open") => open.push(a),
            Some("answered") => {
                if let Some(i) = open.iter().position(|o| o["id"] == a["id"]) {
                    let ask = open.remove(i);
                    out.push((num(&ask, "ts"), num(&a, "ts"), text(&ask, "question")));
                }
            }
            _ => {}
        }
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    out.extend(open.iter().map(|a| (num(a, "ts"), now, text(a, "question"))));
    out
}

/// Marks each step's waits for a human answer (a resumed run's earlier part has no call log, so its waits show only
/// in agg's asks): the part of a step's driver time inside a wait becomes a `human` segment.
fn mark_waits(project: &Path, result: &mut [Value]) {
    for (from, to, question) in waits(project) {
        for step in result.iter_mut() {
            let (lo, hi) = (from.max(num(step, "start")), to.min(num(step, "end")));
            let segs: Vec<Value> = step["segments"].as_array().cloned().unwrap_or_default();
            if hi - lo <= 1.0 || segs.iter().any(|s| s["cat"] == "human" && num(s, "start") < hi && num(s, "end") > lo) {
                continue;
            }
            let mut kept: Vec<Value> = segs.iter().flat_map(|s| outside(s, lo, hi)).collect();
            kept.push(json!({"cat": "human", "label": question.chars().take(80).collect::<String>(), "start": lo, "end": hi}));
            kept.sort_by(|a, b| num(a, "start").total_cmp(&num(b, "start")));
            step["segments"] = json!(kept);
        }
    }
}

/// The segment with one number changed.
fn with(seg: &Value, key: &str, value: f64) -> Value {
    let mut copy = seg.clone();
    copy[key] = json!(value);
    copy
}

/// A driver segment without the time from lo to hi; any other segment as it is.
fn outside(seg: &Value, lo: f64, hi: f64) -> Vec<Value> {
    if seg["cat"] != "driver" || num(seg, "end") <= lo || num(seg, "start") >= hi {
        return vec![seg.clone()];
    }
    let mut out = vec![];
    if num(seg, "start") < lo {
        out.push(with(seg, "end", lo));
    }
    if num(seg, "end") > hi {
        out.push(with(seg, "start", hi));
    }
    out
}

/// Per agg session, the agents and models it ran with (agg's report: worker, and "ruler" the critic).
fn used(project: &Path) -> std::collections::HashMap<u64, Vec<Value>> {
    let report: Value = std::fs::read_to_string(project.join("agg/private/report.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    report["session_log"].as_array().into_iter().flatten().filter_map(|s| Some((s["session"].as_u64()?, s["usage"].as_array().into_iter().flatten()
        .map(|u| json!({"role": if u["role"] == "ruler" { json!("critic") } else { u["role"].clone() }, "agent": u["agent"], "model": u["model"]})).collect())))
        .collect()
}

/// Adds to each step the agents and models its sessions used.
fn models(project: &Path, result: &mut [Value]) {
    let sessions = used(project);
    for s in result.iter_mut() {
        let mut ran: Vec<Value> = vec![];
        for g in s["segments"].as_array().into_iter().flatten() {
            for u in g["session"].as_u64().and_then(|n| sessions.get(&n)).into_iter().flatten() {
                if !ran.contains(u) {
                    ran.push(u.clone());
                }
            }
        }
        if !ran.is_empty() {
            s["used"] = json!(ran);
        }
    }
}

/// Gives each step the files its commit changed since the result before it (the first since its own before-ref).
fn add_files(project: &Path, result: &mut [Value]) {
    let mut before: Option<String> = None;
    for s in result.iter_mut() {
        let key = s["id"].as_str().unwrap_or_default().to_string();
        let since = before.clone().unwrap_or_else(|| format!("refs/agg/before/{key}"));
        let outputs: Vec<String> = match s["commit"].as_str().filter(|c| !c.is_empty()) {
            Some(commit) => std::process::Command::new("git").args(["-C", &project.display().to_string(), "diff", "--name-only", &since, commit])
                .output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect()).unwrap_or_default(),
            None => vec![],
        };
        s["files"] = json!({"outputs": {"files": outputs.iter().take(SHOWN).collect::<Vec<_>>(), "more": outputs.len().saturating_sub(SHOWN)}});
        if let Some(c) = s["commit"].as_str().filter(|c| !c.is_empty()) {
            before = Some(c.to_string());
        }
    }
}

/// The segments as substeps, in time order, without the driver's; consecutive hooks as one.
fn parts(segments: &[Value]) -> Vec<Value> {
    let mut sorted: Vec<&Value> = segments.iter().collect();
    sorted.sort_by(|a, b| num(a, "start").total_cmp(&num(b, "start")));
    let mut out: Vec<Value> = vec![];
    for seg in sorted {
        let cat = seg["cat"].as_str().unwrap_or_default();
        let Some((_, role)) = ROLE.iter().find(|(c, _)| *c == cat) else { continue };
        if *role == "hooks" && out.last().is_some_and(|l| l["role"] == "hooks") {
            fold(out.last_mut().expect("checked"), seg);
        } else {
            let named = ["check", "critique", "hooks", "human"].contains(role);
            out.push(json!({"role": role, "label": if named { text(seg, "label") } else { String::new() }, "start": seg["start"], "end": seg["end"],
                            "secs": ((num(seg, "end") - num(seg, "start")) * 10.0).round() / 10.0, "met": seg["met"], "why": text(seg, "why"),
                            "cost": (num(seg, "cost") * 10000.0).round() / 10000.0, "count": 1}));
        }
    }
    out
}

/// Adds one hook to the folded hooks line: its name and time; a failing hook's verdict and reason win.
fn fold(hooks: &mut Value, seg: &Value) {
    hooks["end"] = seg["end"].clone();
    hooks["secs"] = json!(((num(hooks, "secs") + num(seg, "end") - num(seg, "start")) * 10.0).round() / 10.0);
    hooks["count"] = json!(hooks["count"].as_u64().unwrap_or(1) + 1);
    hooks["label"] = json!(format!("{}, {}", text(hooks, "label"), text(seg, "label")));
    if seg["met"] == false {
        hooks["met"] = json!(false);
        hooks["why"] = json!(format!("{}: {}", text(seg, "label"), text(seg, "why")));
    }
}

/// The substeps after the last result: the running step's, which has no result yet.
fn pending(spans: &[Value], result: &[Value]) -> Vec<Value> {
    let last = result.last().map_or(f64::MIN, |s| num(s, "end"));
    parts(&spans.iter().filter(|s| num(s, "end") > last).cloned().collect::<Vec<_>>())
}

/// The text form of the profile: per step its minutes by role, the totals last.
pub fn render(report: &Value) -> String {
    let mut out: Vec<String> = report["steps"].as_array().into_iter().flatten().map(|s| {
        let mut by: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
        for g in s["segments"].as_array().into_iter().flatten() {
            *by.entry(text(g, "cat")).or_default() += num(g, "end") - num(g, "start");
        }
        let split: Vec<String> = by.iter().map(|(k, v)| format!("{k} {:.1}", v / 60.0)).collect();
        format!("{:6.1} min  {}  [{}]", (num(s, "end") - num(s, "start")) / 60.0, text(s, "name"), split.join(", "))
    }).collect();
    out.push(format!("tokens {}, cost ${:.2}, task force {}", report["tokens"], num(report, "cost"), report["task_force"]));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(kind: &str, ts: u64, extra: Value) -> Value {
        let mut c = json!({"kind": kind, "ts": ts});
        c.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        c
    }

    /// A session's span is its own seconds; the time before it is the driver's; hooks fold into one substep.
    #[test]
    fn a_sessions_span_is_its_own_seconds_and_the_rest_is_the_drivers() {
        let calls = vec![call("note", 100, json!({})),
                         call("step", 200, json!({"outcome": {"step": "a", "secs": 60, "tokens": 10, "cost": 0.5, "session": 1}})),
                         call("judge", 210, json!({"judge": "hook-x", "verdict": {"met": true}, "tokens": 0, "cost": 0.0})),
                         call("judge", 220, json!({"judge": "hook-y", "verdict": {"met": false, "rationale": "bad"}, "tokens": 0, "cost": 0.0})),
                         call("step", 400, json!({"outcome": {"step": "a", "secs": 100, "tokens": 15, "cost": 0.7, "session": 2}}))];
        let s = spans(&calls);
        let cats: Vec<&str> = s.iter().map(|x| x["cat"].as_str().unwrap()).collect();
        assert_eq!(cats, ["driver", "agent", "hooks", "hooks", "driver", "repair"]);
        assert_eq!(s[1]["start"], 140.0);
        assert_eq!(s[5]["tokens"], 5.0, "the growth of the running totals");
        let p = parts(&s);
        assert_eq!(p.iter().map(|x| x["role"].as_str().unwrap()).collect::<Vec<_>>(), ["do", "hooks", "repair"]);
        assert_eq!(p[1]["count"], 2);
        assert_eq!(p[1]["met"], false);
        assert_eq!(p[1]["why"], "hook-y: bad".replace("hook-", ""));
    }

    /// A span goes into the step whose time it ends in, clipped; what no call covers is the driver's.
    #[test]
    fn spans_go_into_the_step_they_end_in_and_the_uncovered_time_is_the_drivers() {
        let mut result = results(&[json!({"step": "a", "kind": "done", "met": true, "rationale": "", "at": 100})], 0.0);
        assign(&mut result, &[json!({"cat": "agent", "label": "s", "start": 20.0, "end": 60.0})]);
        let cats: Vec<String> = result[0]["segments"].as_array().unwrap().iter().map(|s| text(s, "cat")).collect();
        assert_eq!(cats, ["agent", "driver"]);
        assert_eq!(result[0]["segments"][1]["label"], "not in agg's call log");
    }

    /// A wait for a human takes its time out of the step's driver time.
    #[test]
    fn a_wait_for_a_human_is_carved_out_of_the_drivers_time() {
        let driver = json!({"cat": "driver", "label": "d", "start": 0.0, "end": 100.0});
        let parts = outside(&driver, 30.0, 60.0);
        assert_eq!((num(&parts[0], "end"), num(&parts[1], "start")), (30.0, 60.0));
        assert_eq!(outside(&json!({"cat": "agent", "start": 0.0, "end": 100.0}), 30.0, 60.0).len(), 1);
    }

    /// A hook's line gives when, name, verdict and why.
    #[test]
    fn a_hook_line_gives_when_name_and_why() {
        let h = hook(&json!({"rationale": "before hook-declared-inputs: every input exists", "met": true}));
        assert_eq!((h["when"].as_str(), h["name"].as_str(), h["why"].as_str()), (Some("before"), Some("declared inputs"), Some("every input exists")));
        let h = hook(&json!({"rationale": "phase ingest after x: why", "met": false}));
        assert_eq!(h["when"], "phase ingest after");
    }

    /// A step run again after the task force sent the run back is marked, and an idle tool step is inactive.
    #[test]
    fn a_step_again_after_the_task_force_is_marked_and_an_idle_one_inactive() {
        let record = vec![json!({"step": "1.1 a", "kind": "done", "met": true, "rationale": "x", "at": 10}),
                          json!({"step": "1.2 b", "kind": "task-force", "met": true, "rationale": "re-enter at a: x", "at": 20}),
                          json!({"step": "1.1 a", "kind": "done", "met": true, "rationale": "x", "at": 30}),
                          json!({"step": "1.3 c", "kind": "tool", "met": true, "rationale": "it doesn't apply", "at": 40})];
        let r = results(&record, 0.0);
        assert_eq!(r[2]["name"], "1.1 a (again)");
        assert_eq!(r[3]["inactive"], true);
        assert_eq!(phase("6.3-U1 x"), "6");
        assert_eq!(phase("end"), "end");
    }
}
