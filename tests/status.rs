//! The LIVE interfaces, end to end (A-17): `agg status --json` read from another process while a
//! real run is in progress, and `agg answer --json` delivering an answer into the worker's next
//! brief. Every document is validated against its PUBLISHED schema by an independent validator
//! (the `jsonschema` crate) — the claim under test is that the schema says what agg prints.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn wait_for(p: &Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !p.exists() {
        assert!(Instant::now() < deadline, "{} never appeared", p.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A validator for `docs/schemas/<file>`, which must itself be a valid JSON Schema.
fn validator(file: &str) -> jsonschema::Validator {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/schemas").join(file);
    let schema: Value = serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap_or_else(|e| panic!("{file} is not a valid JSON Schema: {e}"))
}

fn assert_valid(file: &str, doc: &Value) {
    let errs: Vec<String> = validator(file).iter_errors(doc).map(|e| format!("{e} at {}", e.instance_path())).collect();
    assert!(errs.is_empty(), "violates docs/schemas/{file}:\n{}\n{doc:#}", errs.join("\n"));
}

/// The worker for the live run. Session N marks `ctl/started_N`, copies its brief to `ctl/brief_N`,
/// and waits for `ctl/go_N` — so the test reads the status mid-session. Session 1 asks a human a
/// `choose` question; session 3 does the work. It reports a model and known token counts.
fn worker(ctl: &Path, agg_bin: &Path) -> String {
    format!(
        r#"#!/bin/sh
for a in "$@"; do [ "$a" = --version ] && {{ echo fake-claude 0.0.0; exit 0; }}; done
ctl='{ctl}'
n=1; while [ -f "$ctl/started_$n" ]; do n=$((n+1)); done
cp agg/private/INSTRUCTIONS.md "$ctl/brief_$n"
printf '%s\n' '{{"type":"system","subtype":"init","model":"reported-model-1","session_id":"s"}}'
[ "$n" = 1 ] && '{agg}' hil choose "Which database?" --option postgres --option sqlite >/dev/null
: > "$ctl/started_$n"
while [ ! -f "$ctl/go_$n" ]; do sleep 0.05; done
if [ "$n" = 3 ]; then : > did_work; git add did_work >/dev/null 2>&1; git commit -qm w >/dev/null 2>&1; fi
printf '%s\n' '{{"type":"result","subtype":"success","result":"ok","usage":{{"input_tokens":10,"output_tokens":50,"cache_creation_input_tokens":200,"cache_read_input_tokens":3000}},"total_cost_usd":0.25}}'
"#,
        ctl = ctl.display(),
        agg = agg_bin.display()
    )
}

/// `(tmp, project dir, control dir, PATH)`.
fn live_project() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, ctl) = (tmp.path().join("proj"), tmp.path().join("ctl"));
    fs::create_dir_all(&ctl).unwrap();
    let claude = tmp.path().join("bin/claude");
    write(&claude, &worker(&ctl, Path::new(env!("CARGO_BIN_EXE_agg"))));
    chmod_x(&claude);
    let judge = dir.join("agg/judges/worked.sh");
    write(&judge, "#!/bin/sh\n[ -f did_work ] && echo '{\"met\":true,\"value\":1,\"max\":1}' || echo '{\"met\":false,\"value\":0,\"max\":1}'\n");
    chmod_x(&judge);
    write(
        &dir.join("agg/agg.yaml"),
        "project: \"live\"\ndefaults: { model: \"configured-model\" }\nsteps:\n  worker: {}\nsequence:\n  steps: [\"worker\"]\n  done_if: \"worked\"\nsummary: { enabled: false }\n",
    );
    write(&agg::paths::agg_dir(&dir).join("STATE.md"), "work\n");
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "base"]);
    let path = format!("{}:{}", tmp.path().join("bin").display(), std::env::var("PATH").unwrap_or_default());
    (tmp, dir, ctl, path)
}

/// `agg <args> --project <dir>`, run from a DIFFERENT directory, as a watching program would.
fn agg_from_outside(dir: &Path, path: &str, args: &[&str]) -> Output {
    let elsewhere = dir.parent().unwrap();
    Command::cargo_bin("agg")
        .unwrap()
        .current_dir(elsewhere)
        .env("PATH", path)
        .env("HOME", elsewhere.join(".home"))
        .args(args)
        .arg("--project")
        .arg(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn status(dir: &Path, path: &str) -> Value {
    let out = agg_from_outside(dir, path, &["status", "--json"]);
    assert!(out.status.success(), "status --json failed: {}", String::from_utf8_lossy(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).expect("status --json prints one JSON document");
    assert_valid("status.v1.json", &v);
    v
}

fn answer(dir: &Path, path: &str, id: &str, value: &str) -> (Option<i32>, Value) {
    let out = agg_from_outside(dir, path, &["answer", id, value, "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).expect("answer --json prints one JSON document");
    assert_valid("answer.v1.json", &v);
    (out.status.code(), v)
}

/// While a run is in progress, `status --json` from another process validates and shows the session
/// advancing, the step, the open ask with its options and the usage so far; an invalid choice is
/// refused with a non-zero exit; a valid one reaches the worker's next brief.
#[test]
fn status_follows_a_live_run_and_an_answer_reaches_the_next_brief() {
    let (_tmp, dir, ctl, path) = live_project();
    let mut run = Command::cargo_bin("agg")
        .unwrap()
        .current_dir(&dir)
        .env("PATH", &path)
        .env("HOME", dir.join(".home"))
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    wait_for(&ctl.join("started_1"));
    let s = status(&dir, &path);
    assert_eq!((s["running"].as_bool(), s["finished"].as_bool(), s["session"].as_u64()), (Some(true), Some(false), Some(1)));
    assert_eq!(s["current_step"]["name"], "worker");
    assert_eq!(s["current_step"]["model"], "configured-model", "the step names what agg asked for");
    assert_eq!(s["stop_reason"], Value::Null);
    assert_eq!(s["judges"]["worked"]["last"]["met"], false, "the baseline verdict is the latest so far");
    assert!(s["run_id"].as_str().is_some_and(|r| r.starts_with("run-")), "{s}");

    fs::write(ctl.join("go_1"), "").unwrap();
    wait_for(&ctl.join("started_2"));
    let s = status(&dir, &path);
    assert_eq!(s["session"], 2, "the session advanced");
    assert_eq!(s["sessions"]["total"], 2);
    let worker = &s["usage"]["worker"];
    assert_eq!(worker["tokens"]["output"], 50, "session 1's usage is in the live totals: {s}");
    assert_eq!((worker["tokens"]["cache_write"].as_u64(), worker["cost_source"].as_str()), (Some(200), Some("reported")));
    let ask = &s["open_asks"][0];
    assert_eq!((ask["kind"].as_str(), ask["question"].as_str()), (Some("choose"), Some("Which database?")));
    assert_eq!(ask["options"], serde_json::json!(["postgres", "sqlite"]));
    let id = ask["id"].as_str().unwrap().to_string();

    let (code, v) = answer(&dir, &path, &id, "mysql");
    assert_eq!(code, Some(1), "an answer off the option list exits non-zero");
    assert_eq!((v["accepted"].as_bool(), v["answer"].is_null()), (Some(false), true));
    assert!(v["reason"].as_str().unwrap().contains("not one of the options"), "{v}");
    let (code, v) = answer(&dir, &path, &id, "2");
    assert_eq!(code, Some(0));
    assert_eq!((v["accepted"].as_bool(), v["answer"].as_str(), v["reason"].is_null()), (Some(true), Some("sqlite"), true));
    assert!(status(&dir, &path)["open_asks"].as_array().unwrap().is_empty(), "an answered ask is no longer open");

    fs::write(ctl.join("go_2"), "").unwrap();
    wait_for(&ctl.join("started_3"));
    let brief = fs::read_to_string(ctl.join("brief_3")).unwrap();
    assert!(brief.contains("Which database?") && brief.contains("**sqlite**"), "the answer reaches the next brief:\n{brief}");
    fs::write(ctl.join("go_3"), "").unwrap();
    assert_eq!(run.wait().unwrap().code(), Some(0));

    let s = status(&dir, &path);
    assert_eq!((s["running"].as_bool(), s["finished"].as_bool(), s["phase"].as_str()), (Some(false), Some(true), Some("done")));
    assert!(s["stop_reason"].is_string());
    assert_eq!(s["usage"]["worker"]["tokens"]["output"], 150, "three sessions × 50");
    assert_eq!(s["judges"]["worked"]["last"]["met"], true);
}

/// Before any run there is no snapshot: `status --json` fails loudly instead of printing an empty
/// document a consumer might mistake for "nothing happening".
#[test]
fn status_json_without_a_run_is_an_error() {
    let (_tmp, dir, _ctl, path) = live_project();
    let out = agg_from_outside(&dir, &path, &["status", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no run snapshot yet"));
}

/// An unknown ask id is refused through the same JSON result.
#[test]
fn answering_an_unknown_ask_is_refused_as_json() {
    let (_tmp, dir, _ctl, path) = live_project();
    let (code, v) = answer(&dir, &path, "nope", "yes");
    assert_eq!(code, Some(1));
    assert_eq!((v["accepted"].as_bool(), v["id"].as_str()), (Some(false), Some("nope")));
    assert!(v["reason"].as_str().unwrap().contains("no ask `nope`"), "{v}");
}
