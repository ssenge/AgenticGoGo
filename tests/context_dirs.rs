//! `context:` end to end — READ-ONLY context directories from outside the project, through the real
//! `agg` binary and a FAKE agent.
//!
//! Two halves. What startup REFUSES (every config agg cannot enforce, located at its key). And what
//! a run DOES on this host's kernel sandbox: the brief lists each directory with a pointer to its
//! `index.md`, the worker and its judge read it, and every way of changing it — a write, a new file,
//! renaming its parent away to put a forged one in its place — is refused by the kernel.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::Path;
use std::process::Command;

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
    Command::new("git").args(args).current_dir(dir).output().unwrap();
}

/// The fake agent: keeps its brief, reads the context through `$AGG_CONTEXT_KB`, then tries each way
/// of changing it and logs the outcome of each.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
cp agg/private/INSTRUCTIONS.md brief.txt
cat "$AGG_CONTEXT_KB/index.md" > read.txt 2>&1
t() { if sh -c "$2" 2>/dev/null; then echo "$1 OK"; else echo "$1 DENIED"; fi; }
{
  t write "echo evil > '$AGG_CONTEXT_KB/index.md'"
  t append "echo evil >> '$AGG_CONTEXT_KB/index.md'"
  t new_file "echo evil > '$AGG_CONTEXT_KB/new.md'"
  t delete "rm '$AGG_CONTEXT_KB/index.md'"
  t rename_self "mv '$AGG_CONTEXT_KB' '$AGG_CONTEXT_KB.moved'"
  t rename_parent "mv \"\$(dirname '$AGG_CONTEXT_KB')\" \"\$(dirname '$AGG_CONTEXT_KB').moved\""
} > attacks.log
echo work > src.txt
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0.01}'
exit 0
"#;

/// The judge reads the context (it must see the same material) and tries to write it.
const JUDGE: &str = r#"#!/bin/sh
if echo judge > "$AGG_CONTEXT_KB/judge.md" 2>/dev/null; then echo JUDGE_WROTE > "$AGG_JUDGE_SCRATCH/j.log"; fi
if grep -q knowledge "$AGG_CONTEXT_KB/index.md" && [ -f src.txt ]; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"read the context"}'
else
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"cannot read the context"}'
fi
"#;

/// A git project whose run names `kb` as a context, under `tier`. Returns the project, the context
/// dir (inside its own temp parent, which the sandbox would otherwise let the worker write) and PATH.
fn project(tier: &str, context: &str) -> (tempfile::TempDir, tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let kb_parent = tempfile::tempdir().unwrap();
    let kb = kb_parent.path().join("kb");
    write(&kb, "index.md", "# knowledge\n");
    let d = tmp.path();
    git(d, &["init", "-q", "-b", "main"]);
    git(d, &["config", "user.email", "t@t"]);
    git(d, &["config", "user.name", "t"]);
    write(d, ".gitignore", "agg/state/\nagg/private/\nbin/\n.home/\nbrief.txt\nread.txt\nattacks.log\n");
    let context = context.replace("{KB}", &kb.display().to_string());
    write(
        d,
        "agg/agg.yaml",
        &format!(
            "project: \"ctx\"\n\
             defaults: {{ model: \"fake\", isolation: \"{tier}\" }}\n\
             steps:\n  worker: {{}}\n\
             sequence:\n  steps: [\"worker\"]\n  done_if: \"read_kb\"\n\
             summary: {{ enabled: false }}\n{context}"
        ),
    );
    write(d, "agg/judges/read_kb.sh", JUDGE);
    chmod_x(&d.join("agg/judges/read_kb.sh"));
    write(d, "agg/state/STATE.md", "do work\n");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "init"]);
    write(d, "bin/claude", FAKE_CLAUDE);
    chmod_x(&d.join("bin/claude"));
    let path = format!("{}:{}", d.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    (tmp, kb_parent, path)
}

fn agg(dir: &Path, path: &str, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("agg").unwrap().args(args).current_dir(dir).env("PATH", path).env("HOME", dir.join(".home")).output().unwrap()
}

fn check_problems(dir: &Path, path: &str) -> Vec<(String, String)> {
    let out = agg(dir, path, &["check", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("check --json prints JSON");
    v["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["path"].as_str().unwrap().to_string(), p["message"].as_str().unwrap().to_string()))
        .collect()
}

const KB: &str = "context:\n  - { path: \"{KB}\", as: \"kb\", mode: \"ro\" }\n";

/// Under `none` nothing would stop a write — so the run is refused, at the key that set the tier.
#[test]
fn a_context_with_an_unconfined_step_is_refused() {
    let (tmp, _kb, path) = project("none", KB);
    let p = check_problems(tmp.path(), &path);
    assert!(p.iter().any(|(at, m)| at == "defaults.isolation" && m.contains("context")), "{p:#?}");
    // …and `agg run` refuses it before any session.
    let out = agg(tmp.path(), &path, &["run", "--max-sessions", "1"]);
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!tmp.path().join("brief.txt").exists(), "no worker ran");
}

/// Every entry agg cannot enforce, each at its own key; `mode` other than `ro` is a parse refusal.
#[test]
fn each_bad_entry_is_refused_at_its_key() {
    let ctx = "context:\n  - { path: \"{KB}\", as: \"kb\" }\n  - { path: \"{KB}\", as: \"kb\" }\n  \
               - { path: \"/no/such/dir/anywhere\", as: \"gone\" }\n  - { path: \"src\", as: \"inside\" }\n  \
               - { path: \"{KB}\", as: \"Bad Name\" }\n";
    let (tmp, _kb, path) = project("sandbox", ctx);
    fs::create_dir_all(tmp.path().join("src")).unwrap();
    let at: Vec<String> = check_problems(tmp.path(), &path).into_iter().map(|(a, _)| a).collect();
    for want in ["context[1].as", "context[2].path", "context[3].path", "context[4].as"] {
        assert!(at.contains(&want.to_string()), "`{want}` must be refused: {at:?}");
    }
    let (tmp, _kb, path) = project("sandbox", "context:\n  - { path: \"{KB}\", as: \"kb\", mode: \"rw\" }\n");
    let p = check_problems(tmp.path(), &path);
    assert!(p.iter().any(|(at, m)| at == "context[0].mode" && m.contains("unknown variant")), "{p:#?}");
}

/// Can this process apply a REAL (restricting) kernel sandbox? A nested one refuses, and then the
/// test is skipped LOUDLY rather than failed.
fn kernel_sandbox_usable() -> bool {
    let probe = if cfg!(target_os = "macos") {
        Command::new("sandbox-exec")
            .args(["-p", "(version 1)(deny default)(allow process*)(allow file-read*)(allow sysctl-read)(allow mach*)", "/usr/bin/true"])
            .output()
    } else {
        Command::new("bwrap").args(["--ro-bind", "/", "/", "--dev", "/dev", "--", "/bin/true"]).output()
    };
    probe.map(|o| o.status.success()).unwrap_or(false)
}

/// THE ACCEPTANCE CASE on the sandbox tier: the brief lists the context with its index, the worker
/// and the judge READ it, and nothing changes it — not a write, a new file, a delete, nor renaming
/// it or its parent away (the parent sits in `$TMPDIR`, which the jail can otherwise write).
#[test]
fn under_the_sandbox_a_context_is_listed_readable_and_never_writable() {
    if !kernel_sandbox_usable() {
        eprintln!("SKIPPED: no usable kernel sandbox on this host (nested, or not installed)");
        return;
    }
    let (tmp, kb_parent, path) = project("sandbox", KB);
    let d = tmp.path();
    let kb = fs::canonicalize(kb_parent.path().join("kb")).unwrap();
    let out = agg(d, &path, &["run", "--max-sessions", "1"]);
    let log = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "the judge read the context, so the run is done:\n{log}");

    let brief = fs::read_to_string(d.join("brief.txt")).unwrap();
    assert!(brief.contains("## Context (read-only)"), "{brief}");
    assert!(brief.contains(&format!("`kb` — `{}` — start at `{}/index.md`", kb.display(), kb.display())), "{brief}");
    assert_eq!(fs::read_to_string(d.join("read.txt")).unwrap(), "# knowledge\n", "the worker reads it");

    let attacks = fs::read_to_string(d.join("attacks.log")).unwrap();
    for a in ["write", "append", "new_file", "delete", "rename_self", "rename_parent"] {
        assert!(attacks.contains(&format!("{a} DENIED")), "`{a}` must be refused by the kernel:\n{attacks}");
    }
    assert_eq!(fs::read_to_string(kb.join("index.md")).unwrap(), "# knowledge\n");
    assert!(!kb.join("new.md").exists() && !kb.join("judge.md").exists(), "nothing new in the context");
    assert!(kb_parent.path().exists(), "the parent was not renamed away");
}
