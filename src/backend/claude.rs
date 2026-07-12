//! The Claude Code backend — `claude -p`.
//!
//! The reference implementation of [`AgentBackend`], and (today) the only one. Everything
//! Claude-specific in the whole crate lives here or in [`super::stream`], which parses its
//! `stream-json` wire format.
//!
//! Claude is the most capable backend of the ones surveyed: it is the only one that reports a
//! DOLLAR cost, and the only one with a verified way to run a genuinely non-agentic one-shot call
//! (tools and MCP off) — which is what an LLM judge has to be, or it could go and edit the thing
//! it is grading. See [`super::Capabilities`] for what that means for its siblings.

use super::{stream, AgentBackend, Capabilities, OneShot, SessionReport, SessionSpec};
use crate::os::proc::{self, Captured};
use anyhow::Result;
use std::path::Path;
use std::process::{Command, Stdio};

/// Default worker model (agg.yaml `model:`).
pub const DEFAULT_MODEL: &str = "claude-opus-4-8[1m]";
/// Default summarizer / LLM-judge model — the cheap one.
pub const DEFAULT_SUMMARY_MODEL: &str = "haiku";

/// The Claude Code CLI, driven headlessly.
pub struct Claude;

impl AgentBackend for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    /// Resolved via PATH, so tests/cli.rs can shim a fake `claude` and an operator can wrap it.
    fn bin(&self) -> &'static str {
        "claude"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            reports_output_tokens: true, // usage.output_tokens on the result event
            reports_cost_usd: true,      // total_cost_usd — Claude prices itself; nobody else does
            supports_resume: true,       // --resume <session_id>
            supports_effort: true,       // --effort
            detects_rate_limits: true,   // terminal result event carries the limit error
            supports_one_shot: true,     // --strict-mcp-config + --setting-sources user
        }
    }

    fn default_model(&self) -> &'static str {
        DEFAULT_MODEL
    }

    fn default_summary_model(&self) -> &'static str {
        DEFAULT_SUMMARY_MODEL
    }

    /// stdin is `/dev/null` so a worker can never block on a TTY read; stdout/stderr are piped
    /// because the event stream IS stdout.
    fn session_command(&self, spec: &SessionSpec) -> Command {
        let mut command = Command::new(self.bin());
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
        // Applied AFTER agg's own flags and BEFORE -p, so an operator can extend the invocation
        // but not clobber its shape.
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

    fn parse_event(&self, line: &str) -> Option<stream::Event> {
        stream::format_event(line)
    }

    /// Claude reports usage ONCE, on its terminal `result` event — so this fires exactly once per
    /// session and the accumulated total equals the reported total. (Contrast Copilot, which
    /// reports per-message; the worker sums either shape identically.)
    fn parse_usage(&self, line: &str) -> Option<u64> {
        stream::output_tokens_from_result(line)
    }

    /// Claude puts the session id on its TERMINAL `result` event (unlike Codex, which puts it on
    /// the first event) — but the method is per-line, so both fit.
    fn parse_session_id(&self, line: &str) -> Option<String> {
        stream::session_id_from_result(line)
    }

    /// Max thinking effort by default — the top of the `-p` flag's enum.
    fn default_effort(&self) -> &'static str {
        "max"
    }

    /// Claude's terminal event is `{"type":"result", …}`, carrying the cost and any rate-limit
    /// error together — so one parse of the line yields the whole report.
    fn parse_result(&self, line: &str) -> Option<SessionReport> {
        stream::parse_result(line)
    }

    /// # trust boundary (verified the hard way — do not "simplify" these flags away)
    /// We deliberately do NOT pass `--bare`: it skips keychain reads, so the call fails with
    /// "Not logged in" — it cannot authenticate. Instead this stays a normal headless call (which
    /// authenticates) and is isolated two ways: `--strict-mcp-config` (no MCP servers) and
    /// `--setting-sources user` (load ONLY the operator's own settings, never the worker-mutated
    /// repo's `.claude/settings.json` or hooks). Together these stop a worker from steering its
    /// own judge via repo config, while preserving auth. (CLAUDE.md auto-discovery in cwd is the
    /// documented residual — see judge.rs's trust-boundary note.)
    fn one_shot(&self, prompt: &str, model: &str, timeout_secs: u64, cwd: Option<&Path>) -> Result<OneShot, String> {
        let mut command = Command::new(self.bin());
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

    fn is_installed(&self) -> bool {
        Command::new(self.bin())
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn preflight(&self) -> Result<()> {
        if !self.is_installed() {
            anyhow::bail!(
                "the Claude Code CLI (`{}`) was not found on your PATH.\n  \
                 AgenticGoGo drives it to run the inner workers. Install it from\n  \
                 https://claude.com/claude-code and make sure `{} --version` works, then retry.",
                self.bin(),
                self.bin()
            );
        }
        Ok(())
    }
}

/// `--output-format json` wraps the answer in an envelope; the model's text is in `.result`.
/// Falls back to the raw bytes when the shape is anything else, so an envelope change degrades to
/// "pass the text through" rather than to an empty verdict.
fn unwrap_envelope(stdout: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(stdout)
        .ok()
        .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(stdout).into_owned())
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
        let cmd = Claude.session_command(&spec);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(cmd.get_program().to_string_lossy(), "claude");
        // operator args land after agg's flags but before -p, and -p is last so the prompt can
        // never be parsed as a flag value.
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
        let cmd = Claude.session_command(&spec);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(!args.contains(&"--effort".to_string()));
        assert!(!args.contains(&"--resume".to_string()));
    }
}
