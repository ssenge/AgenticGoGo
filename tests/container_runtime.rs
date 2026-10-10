//! The container tier as an AGENT RUNTIME, end to end: the real `agg` binary, a REAL container
//! engine, and a test image with a FAKE `claude` baked in (`tests/fixtures/container/fake-agent`).
//!
//! What this proves, per process the run starts:
//! - the WORKER runs inside its image as the project owner, with a fresh `HOME` every session, its
//!   agent's credential by NAME (`ANTHROPIC_API_KEY`), the run's `pass_env` and read-only
//!   `credentials`, the context read-only at `/context/<as>`, `write_deny` and `agg/private/`
//!   read-only — and the judges' homes out of its reach;
//! - SCRIPT judges and the LLM judge's one-shot run in the judges' image (the run's strongest tier —
//!   no opt-out), with the project READ-ONLY, the context visible, writing only their scratch;
//! - a worker that rewrites a judge script cannot reach the host through it;
//! - no credential VALUE ever lands in agg's ledgers, logs, report or output.
//!
//! Skipped LOUDLY without a running engine. Projects live under `target/`, which every engine agg
//! supports can see (a VM-backed one often cannot see `$TMPDIR`).

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const SECRET: &str = "agg-test-secret-value-7f3a";

fn write(dir: &Path, name: &str, content: &str) {
    let p = dir.join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// The fake-agent image, built once per test process. `None` = no engine (the tests skip).
fn image() -> Option<&'static str> {
    static IMAGE: OnceLock<Option<String>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            if !agg::isolation::container_available() {
                return None;
            }
            let tag = "agg-test-fake-agent:1".to_string();
            let ctx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/container/fake-agent");
            let engine = agg::isolation::container_engine().unwrap_or("docker");
            let out = Command::new(engine).args(["build", "-q", "-t", &tag]).arg(&ctx).output().unwrap();
            assert!(out.status.success(), "building the fake-agent image: {}", String::from_utf8_lossy(&out.stderr));
            Some(tag)
        })
        .as_deref()
}

/// A scratch root under `target/` — visible to every engine — removed when dropped.
struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        if std::env::var_os("KEEP").is_none() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn root(name: &str) -> Root {
    let r = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/agg-ctr-test/{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&r);
    fs::create_dir_all(&r).unwrap();
    Root(fs::canonicalize(&r).unwrap())
}

/// The script judge: runs in the judges' image; logs what it can and cannot do into its scratch.
const DONE_JUDGE: &str = r#"#!/bin/sh
{
  { [ -f /.dockerenv ] || grep -qs -e docker -e containerd /proc/1/cgroup; } && echo JUDGE_IN_CONTAINER
  if echo x > judge_probe 2>/dev/null; then echo JUDGE_PROJECT_WRITE_OK; else echo JUDGE_PROJECT_WRITE_DENIED; fi
  [ -f /context/kb/index.md ] && echo JUDGE_CONTEXT_SEEN
  echo m > "$AGG_JUDGE_SCRATCH/m" && echo JUDGE_SCRATCH_OK
} >> "$AGG_JUDGE_SCRATCH/judge.log" 2>&1
n=$(cat src/sessions.log 2>/dev/null | wc -l | tr -d ' ')
if [ "${n:-0}" -ge 2 ]; then
  echo "{\"met\":true,\"value\":$n,\"max\":2,\"target\":2,\"rationale\":\"two sessions\"}"
else
  echo "{\"met\":false,\"value\":${n:-0},\"max\":2,\"target\":2,\"rationale\":\"not yet\"}"
fi
"#;

const REVIEWED: &str = "---\ninputs: []\n---\nDecide whether the work is done.\n";

/// A container-tier project: the worker and both judges in `img`, a read-only context, a
/// `write_deny` oracle, one `pass_env` name and one `credentials` dir. Returns (project, kb, home).
fn project(r: &Root, img: &str, extra_pass: &str) -> (PathBuf, PathBuf, PathBuf) {
    project_with(r, img, extra_pass, "")
}

/// [`project`] with more `isolation_policy` keys (`network: …`).
fn project_with(r: &Root, img: &str, extra_pass: &str, extra_policy: &str) -> (PathBuf, PathBuf, PathBuf) {
    let d = r.0.join("proj");
    let kb = r.0.join("kb");
    let home = r.0.join("home");
    write(&kb, "index.md", "# kb\n");
    write(&home, ".agg-test-cred/token", "file-cred\n");
    git(&r.0, &["init", "-q", "-b", "main", d.to_str().unwrap()]);
    git(&d, &["config", "user.email", "t@t"]);
    git(&d, &["config", "user.name", "t"]);
    write(&d, ".gitignore", "agg/state/\nagg/private/\n");
    write(&d, "tests/oracle.txt", "orig\n");
    write(
        &d,
        "agg/agg.yaml",
        &format!(
            "project: \"ctr\"\n\
             defaults: {{ model: \"fake\", isolation: \"container\", image: \"{img}\", write_deny: [\"tests/**\"] }}\n\
             judge: {{ agent: \"claude\", model: \"fake\" }}\n\
             steps: {{ worker: {{}} }}\n\
             sequence: {{ steps: [\"worker\"], done_if: \"done AND reviewed\" }}\n\
             summary: {{ enabled: false }}\n\
             context: [{{ path: \"{kb}\", as: \"kb\" }}]\n\
             isolation_policy: {{ pass_env: [\"AGG_TEST_PASSED\"{extra_pass}], credentials: [\"~/.agg-test-cred\"]{extra_policy} }}\n",
            kb = kb.display()
        ),
    );
    write(&d, "agg/judges/done.sh", DONE_JUDGE);
    chmod_x(&d.join("agg/judges/done.sh"));
    write(&d, "agg/judges/reviewed.md", REVIEWED);
    write(&d, "agg/state/STATE.md", "work\n");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "init"]);
    (d, kb, home)
}

fn run(d: &Path, home: &Path, envs: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut c = Command::cargo_bin("agg").unwrap();
    c.args(["run", "--max-sessions", "3"])
        .current_dir(d)
        .env("HOME", home)
        .env("ANTHROPIC_API_KEY", SECRET)
        .env("AGG_TEST_PASSED", "1");
    for (k, v) in envs {
        c.env(k, v);
    }
    // HOME is redirected, which would hide the engine's client config (the active context — colima's
    // socket, say); keep pointing the client at the real one.
    if std::env::var_os("DOCKER_CONFIG").is_none() {
        if let Some(h) = std::env::var_os("HOME") {
            c.env("DOCKER_CONFIG", Path::new(&h).join(".docker"));
        }
    }
    let out = c.output().unwrap();
    (out.status.code(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Every file under `dir`, recursively.
fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn the_worker_and_its_judges_run_in_the_image_with_exactly_what_the_run_grants() {
    let Some(img) = image() else {
        eprintln!("SKIPPED: no container engine");
        return;
    };
    let r = root("runtime");
    let (d, kb, home) = project(&r, img, "");
    let (code, log) = run(&d, &home, &[]);
    assert_eq!(code, Some(0), "two sessions meet `done AND reviewed`:\n{log}");

    let w = read(&d.join("src/worker.log"));
    let uid = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&d).unwrap().uid()
    };
    for want in [
        "WORKER_IN_CONTAINER",
        &format!("uid={uid}"),
        "HOME_WRITABLE",
        "KEY_SEEN",
        "PASS_ENV_SEEN",
        "CRED_FILE_SEEN",
        "CRED_WRITE_DENIED",
        "CONTEXT_READ",
        "CONTEXT_WRITE_DENIED",
        "ORACLE_WRITE_DENIED",
        "PRIVATE_WRITE_DENIED",
        "RULER_HOME_WRITE_DENIED",
    ] {
        assert!(w.contains(want), "worker: `{want}` missing\n{w}\n{log}");
    }
    assert_eq!(w.matches("HOME_FRESH").count(), 2, "a fresh home EVERY session:\n{w}");
    assert!(!w.contains("HOME_SURVIVED"), "nothing survives a session in the worker's home:\n{w}");

    let j = read(&d.join("agg/private/container/judge/scratch-2/judge.log"));
    for want in ["JUDGE_IN_CONTAINER", "JUDGE_PROJECT_WRITE_DENIED", "JUDGE_CONTEXT_SEEN", "JUDGE_SCRATCH_OK"] {
        assert!(j.contains(want), "script judge: `{want}` missing\n{j}");
    }
    let o = read(&d.join("agg/private/container/home/ruler/oneshot.log"));
    for want in ["RULER_IN_CONTAINER", "RULER_PROJECT_WRITE_DENIED", "RULER_KEY_SEEN"] {
        assert!(o.contains(want), "LLM judge: `{want}` missing\n{o}");
    }
    assert!(log.contains("IMAGE_RULER_MET"), "the LLM judge's verdict came from the image:\n{log}");

    assert_eq!(read(&d.join("tests/oracle.txt")), "orig\n");
    assert_eq!(read(&kb.join("index.md")), "# kb\n");
    assert_eq!(read(&home.join(".agg-test-cred/token")), "file-cred\n");

    // the credential VALUE is nowhere agg writes, nor in what it printed
    assert!(!log.contains(SECRET), "the secret reached agg's output");
    let mut all = Vec::new();
    files(&d.join("agg/private"), &mut all);
    for f in all {
        let bytes = fs::read(&f).unwrap_or_default();
        assert!(!String::from_utf8_lossy(&bytes).contains(SECRET), "the secret reached {}", f.display());
    }
}

/// The residual the container tier used to document: a worker rewrites a judge script, and the
/// judge then runs on the HOST. Judges run in the container now — the rewritten script finds no
/// host path to write.
#[test]
fn a_rewritten_judge_cannot_reach_the_host() {
    let Some(img) = image() else {
        eprintln!("SKIPPED: no container engine");
        return;
    };
    let r = root("tamper");
    let (d, _kb, home) = project(&r, img, ", \"AGG_TEST_TAMPER\"");
    let escape = r.0.join("escaped-to-host");
    let (_, log) = run(&d, &home, &[("AGG_TEST_TAMPER", escape.to_str().unwrap())]);
    assert!(read(&d.join("src/worker.log")).contains("WORKER_IN_CONTAINER"), "{log}");
    assert!(!escape.exists(), "the rewritten judge ran on the host and wrote {}:\n{log}", escape.display());
}

/// A-7's acceptance on the reference tier: under `allow: [api.anthropic.com]` the worker cannot
/// reach anything else — and the agent still works (its session, both judges and the LLM judge's
/// one-shot all run behind the list), and nothing of the egress network is left behind.
#[test]
fn behind_an_allow_list_the_worker_cannot_reach_out_and_the_run_still_completes() {
    let Some(img) = image() else {
        eprintln!("SKIPPED: no container engine");
        return;
    };
    let r = root("egress");
    let (d, _kb, home) = project_with(&r, img, ", \"AGG_TEST_EGRESS\"", ", network: { allow: [\"api.anthropic.com\"] }");
    let (code, log) = run(&d, &home, &[("AGG_TEST_EGRESS", "1")]);
    assert_eq!(code, Some(0), "the agent still works behind the list:\n{log}");
    let w = read(&d.join("src/worker.log"));
    assert!(w.contains("EGRESS_BLOCKED") && !w.contains("EGRESS_OPEN"), "example.com must be unreachable:\n{w}");
    assert!(w.contains("PROXY_SET"), "the worker is pointed at the proxy:\n{w}");
    // the run removed its relay and its internal network on the way out (the only egress user in
    // this test binary, and cargo runs test binaries one at a time)
    let engine = agg::isolation::container_engine().unwrap();
    for what in [&["ps", "-aq"][..], &["network", "ls", "-q"][..]] {
        let out = Command::new(engine).args(what).args(["--filter", "label=agg.egress"]).output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty(), "left behind ({what:?}): {}", String::from_utf8_lossy(&out.stdout));
    }
}
