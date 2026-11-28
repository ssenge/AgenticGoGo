//! The generic command harness: an agent CLI described under `agents:` runs as a worker, in the YAML loop and, as
//! `Agent::Custom`, from a Rust driver (a driver's own backend is launchable, not refused).

mod common;

use common::Bounded;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

fn sh(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).output_bounded().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// A git project whose agent CLI is `agent.sh`: it commits `did_work` and prints two lines.
fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    fs::write(
        d.join("agent.sh"),
        "#!/bin/sh\necho \"brief: $(cat \"$1\")\" | head -c 60\n: > did_work\ngit add did_work\ngit commit -qm work\necho done\n",
    )
    .unwrap();
    fs::create_dir_all(d.join("rlt/verifiers")).unwrap();
    fs::write(
        d.join("rlt/verifiers/worked.sh"),
        "#!/bin/sh\nif [ -f did_work ]; then m=true; else m=false; fi\nprintf '{\"met\":%s,\"rationale\":\"w\"}\\n' $m\n",
    )
    .unwrap();
    for f in ["agent.sh", "rlt/verifiers/worked.sh"] {
        let mut p = fs::metadata(d.join(f)).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
        fs::set_permissions(d.join(f), p).unwrap();
    }
    fs::write(
        d.join("rlt/rlt.yaml"),
        "project: cmd\nagents:\n  my-cli:\n    command: [\"sh\", \"{workdir}/agent.sh\", \"{brief_file}\"]\n    output: text\ndefaults: { agent: my-cli }\nsteps:\n  worker: {}\nsequence:\n  steps: [worker]\n  done_if: \"worked\"\nsummary: { enabled: false }\n",
    )
    .unwrap();
    fs::write(d.join("rlt/state/STATE.md"), "").ok();
    sh(d, &["init", "-q", "-b", "main"]);
    sh(d, &["config", "user.email", "t@t"]);
    sh(d, &["config", "user.name", "t"]);
    sh(d, &["commit", "-q", "--allow-empty", "-m", "base"]);
    tmp
}

/// A configured command agent drives the loop to done, and its output lines are shown.
#[test]
fn a_configured_command_agent_runs_as_the_worker_in_the_yaml_loop() {
    let tmp = project();
    let out = Command::new(env!("CARGO_BIN_EXE_rlt"))
        .args(["run", "--max-sessions", "2"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path().join(".home"))
        .output_bounded()
        .unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert!(tmp.path().join("did_work").exists() && said.contains("done"), "{said}");
}

/// A name no shipped agent has and no `agents:` entry describes is refused by `rlt check`, naming the configured ones.
#[test]
fn an_unknown_agent_is_refused_and_a_shipped_name_cannot_be_a_command_agent() {
    let tmp = project();
    let cfg = tmp.path().join("rlt/rlt.yaml");
    let text = fs::read_to_string(&cfg).unwrap();
    fs::write(&cfg, text.replace("defaults: { agent: my-cli }", "defaults: { agent: nope }")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).args(["check"]).current_dir(tmp.path()).output_bounded().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && said.contains("unknown agent `nope`") && said.contains("my-cli (configured)"), "{said}");
    fs::write(&cfg, text.replace("  my-cli:", "  claude:")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).args(["check"]).current_dir(tmp.path()).output_bounded().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("is a shipped agent"), "{said}");
}

/// A driver's own backend runs a session: it is launchable (it used to be refused), and it reports no usage.
#[test]
fn a_drivers_own_backend_is_launched_and_reports_no_usage() {
    use relentless::backend::command::{CommandBackend, CommandSpec};
    use relentless::prelude::*;
    let tmp = project();
    let spec: CommandSpec = serde_yaml::from_str("command: [\"sh\", \"{workdir}/agent.sh\", \"{brief_file}\"]\n").unwrap();
    let backend = CommandBackend::new("mine", spec).unwrap();
    let rlt = Relentless::open(tmp.path()).unwrap();
    let outcome = rlt.step(&Step::new("work").agent(Agent::Custom(Arc::new(backend))).prompt("do the work")).unwrap();
    assert_eq!(outcome.exit, 0, "the session ran");
    assert!(tmp.path().join("did_work").exists() || outcome.landed != Landing::Nothing, "{outcome:?}");
    assert_eq!(outcome.tokens, 0, "a command agent reports no tokens unless its lines do");
}

/// A preset (`qwen`) needs no `agents:` entry: it resolves by name, the run says it is experimental, and its stream is
/// read (here from a stand-in `qwen` on the PATH that writes a Claude-like stream).
#[test]
fn a_preset_agent_runs_by_name_and_says_it_is_experimental() {
    let tmp = project();
    let d = tmp.path();
    let bin = d.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        bin.join("qwen"),
        "#!/bin/sh\n[ \"$1\" = --version ] && exit 0\n: > did_work\ngit add did_work\ngit commit -qm work\n\
         echo '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"stand-in said OK\"}]}}'\n\
         echo '{\"type\":\"result\",\"usage\":{\"output_tokens\":3},\"session_id\":\"s\"}'\n",
    )
    .unwrap();
    let mut p = fs::metadata(bin.join("qwen")).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
    fs::set_permissions(bin.join("qwen"), p).unwrap();
    let cfg = d.join("rlt/rlt.yaml");
    let text = fs::read_to_string(&cfg).unwrap();
    let text = text.replace("agents:\n  my-cli:", "agents_unused:\n  my-cli:").replace("agent: my-cli", "agent: qwen");
    fs::write(&cfg, text.replace("agents_unused:\n  my-cli:\n    command: [\"sh\", \"{workdir}/agent.sh\", \"{brief_file}\"]\n    output: text\n", "")).unwrap();
    sh(d, &["add", "-A"]);
    sh(d, &["commit", "-q", "-m", "config"]);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let out = Command::new(env!("CARGO_BIN_EXE_rlt"))
        .args(["run", "--max-sessions", "2"])
        .current_dir(d)
        .env("HOME", d.join(".home"))
        .env("PATH", path)
        .output_bounded()
        .unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert!(said.contains("agent `qwen` is experimental"), "{said}");
    assert!(said.contains("stand-in said OK") && d.join("did_work").exists(), "{said}");
}

/// `examples/mixed-agents`: a command agent works, Claude (scripted) reviews, and the scenario passes without tokens.
#[test]
fn the_mixed_agents_example_plays_through() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/mixed-agents");
    let out = Command::new(env!("CARGO_BIN_EXE_rlt"))
        .arg("process")
        .arg(root.join("process.yaml"))
        .arg("--scenario")
        .arg(root.join("scenario.yaml"))
        .arg("--projects")
        .arg(root.join("projects"))
        .output_bounded()
        .unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && said.contains("PASS scenario"), "{said}");
}
