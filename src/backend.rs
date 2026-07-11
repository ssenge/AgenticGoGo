//! The agent backend — **the one module that knows which agent binary we drive.**
//!
//! Everything Claude-specific funnels through here: the binary name, the flag vocabulary, the
//! JSON envelope its `--output-format json` wraps answers in, the model defaults, and the
//! install check. Before this module those details were smeared across five files — worker.rs
//! built the interactive invocation, judge.rs and summary.rs each built a near-identical
//! one-shot call (with their own copy of the `.result` unwrap), and doctor.rs and main.rs each
//! had their own byte-for-byte copy of the `--version` probe.
//!
//! # seam
//! Two planned features slot in HERE and nowhere else:
//!   • **a second agent backend** (Codex, Amp, Gemini, …). The three fns below are deliberately
//!     shaped like the methods of the `trait AgentBackend` that will exist once backend #2 is
//!     real — extracting the trait then is a mechanical `impl`, not a refactor. The trait is NOT
//!     defined now: one implementation does not need an abstraction, and guessing the second
//!     one's shape before it exists is how you get the wrong seam.
//!   • **`agg run --sandbox`**: a sandbox is a `Command` wrapper, so it wraps what
//!     [`session_command`] returns.
//!
//! `stream.rs` is the other half of this backend — it parses Claude's `stream-json` events. It
//! stays a separate module for size, but it is backend-private in spirit and moves behind the
//! trait together with this file.

/// Parses the agent's `stream-json` events — the reading half of this backend (see the `# seam`
/// note above; it moves behind the trait together with this file).
pub mod stream;
/// Supervises one worker session: spawns what [`session_command`] builds, then runs the stream
/// reader, the heartbeat and the watchdog over it, and reaps what it leaves behind.
pub mod worker;

use crate::os::proc::{self, Captured};
use anyhow::Result;
use std::path::Path;
use std::process::{Command, Stdio};

/// The agent CLI we drive. Resolved via PATH, so the test suite can shim a fake `claude`
/// (tests/cli.rs does exactly that) and so does any operator with a wrapper script.
pub const BIN: &str = "claude";

/// Default worker model (agg.yaml `model:`).
pub const DEFAULT_MODEL: &str = "claude-opus-4-8[1m]";

/// Default summarizer/LLM-judge model — the cheap one (agg.yaml `summary.model:`).
pub const DEFAULT_SUMMARY_MODEL: &str = "haiku";

// ---------------- the interactive worker session ----------------

/// Everything the backend needs to build one worker invocation. Fields are agent-agnostic on
/// purpose (a model name, an effort level, a resume handle, pass-through args) — the mapping
/// onto *Claude's* flags happens in [`session_command`] and nowhere else.
pub struct SessionSpec<'a> {
    pub prompt: &'a str,
    pub model: &'a str,
    /// thinking effort; empty = don't pass the flag at all.
    pub effort: &'a str,
    /// continue a prior session's context instead of starting fresh.
    pub resume_id: Option<&'a str>,
    /// operator-supplied extra flags (agg.yaml `worker_args`).
    pub extra_args: &'a [String],
    /// the project directory the worker runs in.
    pub cwd: &'a Path,
}

/// Build the `Command` for one interactive worker session. The caller spawns it — process-group
/// setup, the stdout stream reader, the heartbeat and the watchdog are agg's process-management
/// concerns, not the backend's, and they live in worker.rs.
///
/// stdin is `/dev/null` so a worker can never block on a TTY read; stdout/stderr are piped
/// because the event stream IS stdout.
pub fn session_command(spec: &SessionSpec) -> Command {
    let mut command = Command::new(BIN);
    command
        .arg("--dangerously-skip-permissions")
        .arg("--model")
        .arg(spec.model)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose");
    // Max thinking effort for the headless worker (agg.yaml `effort:`; default "max").
    // `ultracode` is interactive-only and not a valid `-p` flag value, so workers get the
    // highest effort reachable from `-p` here and opt into subagent orchestration through the
    // prompt prefix instead.
    if !spec.effort.is_empty() {
        command.arg("--effort").arg(spec.effort);
    }
    if let Some(id) = spec.resume_id {
        command.arg("--resume").arg(id);
    }
    // Applied AFTER agg's own flags and BEFORE -p, so an operator can extend the invocation but
    // not clobber its shape.
    for a in spec.extra_args {
        command.arg(a);
    }
    command
        .arg("-p")
        .arg(spec.prompt)
        .current_dir(spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

// ---------------- the one-shot call (judge + summarizer) ----------------

/// A completed one-shot call: the model's TEXT (already unwrapped from the agent's JSON
/// envelope), plus the exit status and stderr the caller needs to tell "the model said no" from
/// "the call itself failed".
pub struct OneShot {
    /// the model's answer text — envelope already stripped.
    pub body: String,
    pub stderr: Vec<u8>,
    pub success: bool,
}

/// Run a single non-interactive prompt and return the model's answer. The judge and the
/// summarizer both go through this — they used to build near-identical `Command`s and each
/// carried its own copy of the `.result` unwrap below.
///
/// `cwd` is `Some` when the call must see the project (the judge inspects the repo) and `None`
/// when it must not care (the summarizer only reads text it was handed).
///
/// # trust boundary (verified the hard way — do not "simplify" these flags away)
/// We deliberately do NOT pass `--bare`: it skips keychain reads, so the call fails with
/// "Not logged in" — it cannot authenticate. Instead this stays a normal headless call (which
/// authenticates) and is isolated two ways: `--strict-mcp-config` (no MCP servers) and
/// `--setting-sources user` (load ONLY the operator's own settings, never the worker-mutated
/// repo's `.claude/settings.json` or hooks). Together these stop a worker from steering its own
/// judge via repo config, while preserving auth. (CLAUDE.md auto-discovery in cwd is the
/// documented residual — see judge.rs's trust-boundary note.)
pub fn one_shot(prompt: &str, model: &str, timeout_secs: u64, cwd: Option<&Path>) -> Result<OneShot, String> {
    let mut command = Command::new(BIN);
    command
        .arg("-p")
        .arg(prompt)
        .arg("--model")
        .arg(model)
        .arg("--output-format")
        .arg("json")
        .arg("--strict-mcp-config")
        .arg("--setting-sources")
        .arg("user")
        .stdin(Stdio::null());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let out: Captured = proc::run_with_timeout(command, timeout_secs)?;
    Ok(OneShot { body: unwrap_envelope(&out.stdout), stderr: out.stderr, success: out.success })
}

/// `--output-format json` wraps the answer in an envelope; the model's text is in `.result`.
/// Falls back to the raw bytes when the shape is anything else, so an envelope change degrades
/// to "pass the text through" rather than to an empty verdict.
fn unwrap_envelope(stdout: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(stdout)
        .ok()
        .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(stdout).into_owned())
}

// ---------------- install check ----------------

/// Is the agent CLI on PATH and runnable? The cheap probe both `agg doctor` and `agg run`'s
/// preflight use.
pub fn is_installed() -> bool {
    Command::new(BIN)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Hard preflight for `agg run`: bail with an install hint if the agent CLI is missing.
/// `agg doctor` reports the same condition as one line of its checklist via [`is_installed`].
pub fn preflight() -> Result<()> {
    if !is_installed() {
        anyhow::bail!(
            "the Claude Code CLI (`{BIN}`) was not found on your PATH.\n  \
             AgenticGoGo drives it to run the inner workers. Install it from\n  \
             https://claude.com/claude-code and make sure `{BIN} --version` works, then retry."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The envelope unwrap was duplicated in judge.rs and summary.rs and tested in neither.
    #[test]
    fn unwrap_envelope_extracts_result_and_falls_back() {
        assert_eq!(unwrap_envelope(br#"{"result":"the answer","cost":1}"#), "the answer");
        // unexpected shape → pass the raw text through rather than yield nothing
        assert_eq!(unwrap_envelope(br#"{"unexpected":true}"#), r#"{"unexpected":true}"#);
        assert_eq!(unwrap_envelope(b"not json at all"), "not json at all");
        assert_eq!(unwrap_envelope(b""), "");
    }

    #[test]
    fn session_command_shape() {
        let spec = SessionSpec {
            prompt: "do the thing",
            model: "m",
            effort: "max",
            resume_id: Some("abc"),
            extra_args: &["--add-dir".to_string(), "/x".to_string()],
            cwd: Path::new("/tmp"),
        };
        let cmd = session_command(&spec);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(cmd.get_program().to_string_lossy(), BIN);
        // operator args land after agg's flags but before -p, and -p is last so the prompt
        // can never be parsed as a flag value.
        let p = args.iter().position(|a| a == "-p").expect("-p present");
        let add_dir = args.iter().position(|a| a == "--add-dir").expect("worker_args passed through");
        assert!(add_dir < p, "worker_args must precede -p");
        assert_eq!(args.last().unwrap(), "do the thing", "prompt is the final arg");
        assert!(args.contains(&"--resume".to_string()) && args.contains(&"abc".to_string()));
        assert!(args.contains(&"--effort".to_string()));
    }

    /// An empty `effort` must omit the flag entirely — passing `--effort ""` is an error.
    #[test]
    fn empty_effort_omits_the_flag() {
        let spec = SessionSpec {
            prompt: "p",
            model: "m",
            effort: "",
            resume_id: None,
            extra_args: &[],
            cwd: Path::new("/tmp"),
        };
        let cmd = session_command(&spec);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(!args.contains(&"--effort".to_string()));
        assert!(!args.contains(&"--resume".to_string()));
    }
}
