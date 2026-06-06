//! Generic lifecycle hooks + prompt-include composition.
//!
//! agg is TOOL-AGNOSTIC: it never references engram / graphify / rtk / any specific tool. It
//! just (a) runs the user's shell commands at lifecycle moments and (b) prepends the user's
//! text files to the worker prompt. Whatever cross-session machinery a user wants — a code
//! graph, a memory cache, a linter — they wire in via these hooks in `agg.yaml`. A user with
//! no such tools leaves them empty and nothing happens.

use std::path::Path;
use std::process::{Command, Stdio};

/// Run a list of hook commands (in order) from `dir`, foreground, inheriting stdio so their
/// output lands in the loop log. Best-effort: a failing hook is logged, not fatal (a hook is
/// auxiliary tooling, not the loop's core job). `label` names the phase for the log line.
pub fn run(label: &str, cmds: &[String], dir: &Path) {
    for cmd in cmds {
        eprintln!("  [hook:{label}] $ {cmd}");
        let status = shell(cmd, dir).stdout(Stdio::inherit()).stderr(Stdio::inherit()).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("  [hook:{label}] exited {:?} (non-fatal)", s.code()),
            Err(e) => eprintln!("  [hook:{label}] failed to spawn: {e} (non-fatal)"),
        }
    }
}

/// Spawn long-lived `background` hooks detached from the foreground but INSIDE the agg loop's
/// process group, so the straggler reaper (and a group kill on stop) cleans them up — a
/// `--watch` can't leak. Returns nothing; lifetimes are bounded by the loop's group.
pub fn spawn_background(cmds: &[String], dir: &Path) {
    for cmd in cmds {
        eprintln!("  [hook:background] $ {cmd}");
        // stdout/stderr to null: a watcher is chatty and not the loop's source of truth.
        let spawned = shell(cmd, dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        match spawned {
            Ok(_child) => { /* stays in our process group; reaped on stop */ }
            Err(e) => eprintln!("  [hook:background] failed to spawn: {e} (non-fatal)"),
        }
    }
}

/// Concatenate `prompt_includes` files (in order) into one block to prepend to the worker
/// prompt. Missing files are skipped with a log note (not fatal). Returns "" if none.
pub fn gather_prompt_includes(includes: &[String], dir: &Path) -> String {
    let mut out = String::new();
    for path in includes {
        match std::fs::read_to_string(dir.join(path)) {
            Ok(text) => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&text);
            }
            Err(e) => eprintln!("  [prompt_includes] skipping {path}: {e}"),
        }
    }
    out
}

#[cfg(unix)]
fn shell(cmd: &str, dir: &Path) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd).current_dir(dir);
    c
}

#[cfg(not(unix))]
fn shell(cmd: &str, dir: &Path) -> Command {
    let mut c = Command::new("cmd");
    c.arg("/C").arg(cmd).current_dir(dir);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_concatenates_in_order_and_skips_missing() {
        let dir = std::env::temp_dir().join(format!("agg-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "AAA").unwrap();
        std::fs::write(dir.join("b.md"), "BBB").unwrap();
        let got = gather_prompt_includes(
            &["a.md".into(), "missing.md".into(), "b.md".into()], &dir);
        assert_eq!(got, "AAA\n\nBBB"); // ordered, missing skipped
        assert!(gather_prompt_includes(&[], &dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_executes_commands_and_tolerates_failure() {
        let dir = std::env::temp_dir().join(format!("agg-hooks-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // a hook that writes a marker file, and one that fails — neither should panic.
        run("test", &[format!("touch {}", dir.join("ran").display()), "false".into()], &dir);
        assert!(dir.join("ran").exists(), "hook command should have run");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
