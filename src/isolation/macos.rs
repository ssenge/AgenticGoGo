//! macOS OS sandbox via `sandbox-exec` (Seatbelt).
//!
//! Recipe (`internal/ISOLATION.md` §6 / §10.5) — a `(deny default)` profile that reads everything,
//! writes only cwd + `$TMPDIR` + the agent's state dirs, and leaves the network open:
//! ```scheme
//! (version 1)(deny default)
//! (allow process*)
//! (allow file-read*)
//! (allow file-write* (subpath "<cwd>") (subpath "<TMPDIR>") (subpath "<w>")…)
//! (allow network*)(allow mach*)(allow sysctl-read)(allow iokit-open)
//! ```
//! The `mach*`/`sysctl`/`iokit` allowances are the fiddly bits that let a node-based CLI actually
//! run under Seatbelt.
//!
//! Honest caveat: `sandbox-exec` is technically DEPRECATED (it prints a notice) but still works on
//! current macOS and is battle-tested (Chrome, and Claude Code itself sandbox this way). The
//! strategy stays swappable so a future macOS drop can move to the Seatbelt API or fall back.

use anyhow::Result;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Is `sandbox-exec` present and runnable? (`-n` names a builtin profile; a bad one still proves
/// the binary exists and parses args — we only need "is the tool here".)
pub fn available() -> bool {
    // `sandbox-exec` with no args prints usage and exits non-zero, so probe via `which`-style
    // resolution instead: try to run it with `-p` on a trivial always-allow profile against `true`.
    Command::new("sandbox-exec")
        .arg("-p")
        .arg("(version 1)(allow default)")
        .arg("true")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build the `sandbox-exec -p '<profile>' -- prog args…` wrapper.
pub fn build(cwd: &Path, writable: &[PathBuf], prog: &OsStr, args: &[OsString]) -> Result<Command> {
    // `$TMPDIR` is always writable (falling back to `/tmp` if unset).
    let tmp = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    let profile = profile(cwd, &tmp, writable);
    let mut cmd = Command::new("sandbox-exec");
    cmd.arg("-p").arg(profile).arg("--").arg(prog);
    for a in args {
        cmd.arg(a);
    }
    Ok(cmd)
}

/// Assemble the Seatbelt profile: deny-default, read-all, write only the whitelisted subpaths,
/// network open. `tmp` (the resolved `$TMPDIR`) is always in the writable set. Taken as a param —
/// not read from env here — so tests need not mutate the process-wide `TMPDIR` (which would race
/// every other test that writes to a temp dir).
fn profile(cwd: &Path, tmp: &Path, writable: &[PathBuf]) -> String {
    let mut subpaths = vec![cwd.to_path_buf()];
    subpaths.push(tmp.to_path_buf());
    subpaths.extend(writable.iter().cloned());

    let writes: String = subpaths
        .iter()
        .map(|p| format!("(subpath \"{}\")", escape(&p.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(" ");

    format!(
        "(version 1)(deny default)\
         (allow process*)\
         (allow file-read*)\
         (allow file-write* {writes})\
         (allow network*)(allow mach*)(allow sysctl-read)(allow iokit-open)"
    )
}

/// Escape a path for a double-quoted Seatbelt string literal: backslash and double-quote.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_confines_writes_to_cwd_and_tmp_and_state() {
        // tmp is passed explicitly — no global env mutation, so this can't race parallel tests.
        let p = profile(Path::new("/repo/proj"), Path::new("/var/folders/xy"), &[PathBuf::from("/home/u/.claude")]);
        assert!(p.contains("(deny default)"));
        assert!(p.contains("(allow file-read*)"), "reads everything");
        assert!(p.contains("(subpath \"/repo/proj\")"), "cwd is writable");
        assert!(p.contains("(subpath \"/var/folders/xy\")"), "TMPDIR is writable");
        assert!(p.contains("(subpath \"/home/u/.claude\")"), "agent state dir is writable");
        assert!(p.contains("(allow network*)"), "network is open");
    }

    #[test]
    fn escape_quotes_and_backslashes() {
        assert_eq!(escape(r#"/a b/"x""#), r#"/a b/\"x\""#);
        assert_eq!(escape(r"/a\b"), r"/a\\b");
    }
}
