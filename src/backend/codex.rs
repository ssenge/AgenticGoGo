//! The OpenAI Codex backend — `codex exec`.
//!
//! Field paths are from the wire and from Codex's own Rust source
//! (`codex-rs/exec/src/exec_events.rs`), not from prose. The captured stream is at
//! `tests/fixtures/agent-streams/codex-0.144.1.jsonl` and the parser tests run against it.
//!
//! Verified against `codex-cli 0.144.1`:
//!
//! ```text
//!   thread.started    thread_id                    the RESUME HANDLE — first event, not the last
//!   item.started      item.type/…                  a step beginning
//!   item.completed    item.type = agent_message    data in `text`
//!                                = reasoning       the model thinking
//!                                = command_execution / file_change / mcp_tool_call
//!   turn.completed    usage.output_tokens          TERMINAL (success) + usage
//!   turn.failed       error.message                TERMINAL (failure)
//!   error             message                      TERMINAL — a bare error with NO turn wrapper
//! ```
//!
//! # Three things that will catch you out
//! 1. **The prompt is POSITIONAL.** `codex exec [OPTIONS] [PROMPT]`. `-p` means `--profile` here —
//!    passing the prompt to `-p` would send the whole thing as a config-profile NAME.
//! 2. **stdin must be `/dev/null`.** `codex exec` blocks on stdin EOF when spawned as a non-TTY
//!    child ("Reading additional input from stdin…") and hangs forever. Confirmed the hard way.
//! 3. **The session id is on the FIRST event**, `thread.started` — not the terminal one. Hence
//!    [`AgentBackend::parse_session_id`] is per-line.

use super::{stream, AgentBackend, Capabilities, OneShot, SessionReport, SessionSpec};
use crate::util::truncate;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};

/// EMPTY on purpose: let Codex choose its own model, and do not pass `--model` at all.
///
/// Naming a default here is actively harmful. `gpt-5-codex` looks like the obvious choice and
/// fails at runtime with *"The 'gpt-5-codex' model is not supported when using Codex with a ChatGPT
/// account"* — the model a Codex user may use depends on how they AUTHENTICATED (ChatGPT plan vs.
/// API key), which agg cannot know. Codex already picks a model appropriate to the account; an
/// operator who wants a specific one sets `model:` in agg.yaml explicitly.
pub const DEFAULT_MODEL: &str = "";
/// The summarizer/judge model. EMPTY on purpose: Codex must not be handed a model name (a hard
/// 400 on a ChatGPT account), so the flag is omitted entirely and Codex picks its own.
pub const DEFAULT_SUMMARY_MODEL: &str = "";

pub struct Codex;

impl AgentBackend for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn bin(&self) -> &'static str {
        "codex"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // turn.completed.usage.output_tokens
            reports_output_tokens: true,
            // NO. Confirmed twice: no cost/price/usd field exists anywhere in Codex's event
            // schema (checked its Rust source AND grepped a real stream). `over_cost` is refused.
            reports_cost_usd: false,
            // `codex exec resume <SESSION_ID>`, id from thread.started.thread_id.
            supports_resume: true,
            // YES — not via a flag (`codex exec` has none) but via the `-c model_reasoning_effort=`
            // config override, which is verified to work. See `effort_arg` for the level mapping.
            supports_effort: true,
            // YES — best-effort, exactly as it is for Claude. Codex has no error KIND (its
            // ThreadErrorEvent is just `{message: String}`), so a rate limit is only recognisable
            // from the text. That is also true of Claude, whose detector is a substring match on
            // the same kind of prose. Scanning is tightly gated to the TERMINAL failure events
            // (turn.failed / bare error), never tool output, so a command that merely prints "429"
            // cannot trip a false backoff.
            detects_rate_limits: true,
            // YES — via `--sandbox read-only`, which is the property a judge actually needs.
            //
            // The requirement was never "tools must be absent". It is "the judge must not be able
            // to MODIFY the artifact it is grading" — a judge SHOULD read the repo; that is its
            // job. Read-only is exactly that, and it is enforced by the sandbox rather than by
            // hoping the model behaves. (Claude gets the same property a different way: its
            // one-shot call omits `--dangerously-skip-permissions`, so tool execution is denied.)
            supports_one_shot: true,
        }
    }

    fn default_model(&self) -> &'static str {
        DEFAULT_MODEL
    }
    fn default_summary_model(&self) -> &'static str {
        DEFAULT_SUMMARY_MODEL
    }

    /// `high` — the most reasoning Codex offers. (agg's blanket `max` is Claude's vocabulary; see
    /// [`effort_arg`] for how the levels map.)
    fn default_effort(&self) -> &'static str {
        "high"
    }

    /// `codex exec [OPTIONS] <PROMPT>` — prompt POSITIONAL and LAST; stdin nulled or it hangs.
    ///
    /// Resume restructures the argv entirely (`codex exec resume <ID> <PROMPT>`) rather than adding
    /// a flag. That is why this returns a whole `Command`: a backend must be able to reshape argv,
    /// not merely append to it.
    fn session_command(&self, spec: &SessionSpec) -> Command {
        let mut command = Command::new(self.bin());
        command.arg("exec");
        if let Some(id) = spec.resume_id {
            command.arg("resume").arg(id); // SUBCOMMAND, not a flag
        }
        command
            .arg("--json")
            // headless: never prompt for approval, and don't refuse to run outside a git repo.
            .arg("--dangerously-bypass-approvals-and-sandbox")
            .arg("--skip-git-repo-check");
        // Only pass --model if the operator actually named one. Empty = let Codex decide, which is
        // the only safe default: the models available depend on how the user authenticated, and a
        // wrong one is a hard 400 at runtime. See DEFAULT_MODEL.
        if !spec.model.is_empty() {
            command.arg("--model").arg(spec.model);
        }
        // Codex has no `--effort` flag; reasoning effort is a CONFIG override.
        if let Some(level) = effort_arg(spec.effort) {
            command.arg("-c").arg(format!("model_reasoning_effort={level}"));
        }
        for a in spec.extra_args {
            command.arg(a);
        }
        command
            .arg(spec.prompt) // POSITIONAL, last. `-p` is `--profile` here — do not use it.
            .current_dir(spec.cwd)
            .stdin(Stdio::null()) // non-negotiable: codex blocks on stdin EOF as a non-TTY child
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn parse_event(&self, line: &str) -> Option<stream::Event> {
        let v: Value = serde_json::from_str(line).ok()?;
        match v.get("type")?.as_str()? {
            "item.completed" | "item.started" => {
                let item = v.get("item")?;
                let kind = item.get("type")?.as_str()?;
                match kind {
                    "agent_message" | "reasoning" => {
                        let text = clean(item.get("text")?.as_str()?);
                        if text.is_empty() {
                            return None;
                        }
                        Some(think(text))
                    }
                    "command_execution" => {
                        let cmd = item.get("command").and_then(|c| c.as_str()).unwrap_or("(cmd)");
                        Some(tool(format!("$ {}", clean(cmd))))
                    }
                    "file_change" => {
                        let paths: Vec<String> = item
                            .get("changes")
                            .and_then(|c| c.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|c| c.get("path").and_then(|p| p.as_str()))
                                    .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
                                    .collect()
                            })
                            .unwrap_or_default();
                        Some(tool(format!("edit {}", paths.join(" "))))
                    }
                    "mcp_tool_call" => Some(tool("mcp tool".to_string())),
                    "error" => {
                        let m = item.get("message").and_then(|m| m.as_str()).unwrap_or("error");
                        Some(tool_result(format!("ERR {}", clean(m))))
                    }
                    _ => None,
                }
            }
            "turn.completed" => Some(result_event("RESULT ok".to_string())),
            "turn.failed" => {
                let m = v
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("failed");
                Some(result_event(format!("RESULT failed: {}", clean(m))))
            }
            // A bare top-level `error` can END the stream with no turn wrapper at all.
            "error" => {
                let m = v.get("message").and_then(|m| m.as_str()).unwrap_or("error");
                Some(tool_result(format!("ERR {}", clean(m))))
            }
            _ => None,
        }
    }

    /// `turn.completed.usage.output_tokens` — reported ONCE per turn, like Claude.
    ///
    /// `reasoning_output_tokens` is deliberately NOT added. Codex's source documents neither
    /// relationship, but the wire settles it: a real run reported `input_tokens: 24695` with
    /// `cached_input_tokens: 22528` — cached is plainly a SUBSET of input, not a separate bucket.
    /// By the same construction (and OpenAI's Responses-API convention, where reasoning tokens are
    /// counted within output tokens), `reasoning_output_tokens` is a breakdown of `output_tokens`.
    /// Summing them would DOUBLE-COUNT and make the budget guard fire early.
    fn parse_usage(&self, line: &str) -> Option<u64> {
        let v: Value = serde_json::from_str(line).ok()?;
        if v.get("type")?.as_str()? != "turn.completed" {
            return None;
        }
        v.get("usage")?.get("output_tokens")?.as_u64()
    }

    /// From `thread.started` — the FIRST event, not the terminal one. This is the whole reason
    /// [`AgentBackend::parse_session_id`] is per-line rather than a field on [`SessionReport`].
    fn parse_session_id(&self, line: &str) -> Option<String> {
        let v: Value = serde_json::from_str(line).ok()?;
        if v.get("type")?.as_str()? != "thread.started" {
            return None;
        }
        v.get("thread_id")?.as_str().map(str::to_string)
    }

    /// Codex has THREE terminal shapes: `turn.completed` (success), `turn.failed`, and a bare
    /// top-level `error` with no turn wrapper. Treating only the first as terminal loses the
    /// failure paths — though process exit remains ground truth regardless.
    fn parse_result(&self, line: &str) -> Option<SessionReport> {
        let v: Value = serde_json::from_str(line).ok()?;
        let ty = v.get("type")?.as_str()?;
        if !matches!(ty, "turn.completed" | "turn.failed" | "error") {
            return None;
        }
        Some(SessionReport {
            cost_usd: None, // Codex reports no cost, anywhere — see capabilities()
            rate_limited: rate_limited(&v, ty),
        })
    }

    /// A judging / summarizing call: the model may READ, but it cannot WRITE.
    ///
    /// # trust boundary
    /// Two things are defended, mirroring what Claude's one-shot does:
    /// - **`--sandbox read-only`** — the judge can inspect the repo (its job) but cannot modify the
    ///   artifact it is grading. Enforced by the sandbox, not by asking the model nicely.
    /// - **`--ignore-user-config --ignore-rules`** — do NOT load `AGENTS.md` or any repo rules. The
    ///   worker writes those files, so loading them would let the worker reconfigure its own judge.
    ///   This is Codex's equivalent of Claude's `--setting-sources user`.
    ///
    /// `--ephemeral` keeps a judge call from polluting the resumable-session history.
    fn one_shot(&self, prompt: &str, model: &str, timeout_secs: u64, cwd: Option<&Path>) -> Result<OneShot, String> {
        let mut command = Command::new(self.bin());
        command
            .arg("exec")
            .arg("--json")
            .arg("--sandbox")
            .arg("read-only") // CANNOT write — the judge's core guarantee
            .arg("--skip-git-repo-check")
            .arg("--ignore-user-config") // the worker must not steer its own judge
            .arg("--ignore-rules")
            .arg("--ephemeral");
        if !model.is_empty() {
            command.arg("--model").arg(model);
        }
        command.arg(prompt).stdin(Stdio::null()); // positional prompt; stdin nulled or it hangs
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        let out = crate::os::proc::run_with_timeout(command, timeout_secs)?;
        let (output_tokens, cost_usd) = self.tally_one_shot(&out.stdout);
        Ok(OneShot {
            body: last_agent_message(&out.stdout),
            stderr: out.stderr,
            success: out.success,
            output_tokens,
            cost_usd,
        })
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
                // PARALLEL with claude.rs / copilot.rs — install, login, HEADLESS check.
                "the OpenAI Codex CLI (`{}`) was not found on your PATH.\n  \
                 AgenticGoGo drives it to run the inner workers. Install it with\n    \
                 npm install -g @openai/codex\n  \
                 then run `codex login` (or set OPENAI_API_KEY), and make sure\n  \
                 `codex exec \"hello\"` answers.",
                self.bin()
            );
        }
        Ok(())
    }
}

fn think(text: String) -> stream::Event {
    stream::Event {
        display: format!("💬 {}", truncate(&text, 200)),
        kind: stream::EventKind::Think,
        text: truncate(&text, 200),
        is_result: false,
        thought: Some(text),
    }
}
fn tool(text: String) -> stream::Event {
    stream::Event {
        display: format!("🔧 {}", truncate(&text, 200)),
        kind: stream::EventKind::Tool,
        text: truncate(&text, 200),
        is_result: false,
        thought: None,
    }
}
fn tool_result(text: String) -> stream::Event {
    stream::Event {
        display: format!("↳ {}", truncate(&text, 200)),
        kind: stream::EventKind::ToolResult,
        text: truncate(&text, 200),
        is_result: false,
        thought: None,
    }
}
fn result_event(text: String) -> stream::Event {
    stream::Event {
        display: format!("✅ {text}"),
        kind: stream::EventKind::Result,
        text,
        is_result: true,
        thought: None,
    }
}

fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Map agg's effort level onto Codex's `model_reasoning_effort` value. `None` = pass nothing.
///
/// agg speaks Claude's vocabulary (`low|medium|high|xhigh|max`); Codex's tops out at `high`. The
/// two above it are CLAMPED to `high` rather than rejected: "max" means *"think as hard as this
/// agent can"*, and `high` is as hard as Codex thinks. That is an honest clamp, not a silent
/// downgrade — asking for more reasoning than the model offers can only ever give you its most.
fn effort_arg(effort: &str) -> Option<&'static str> {
    match effort {
        "" => None,
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        // Codex has no xhigh/max — clamp to its ceiling.
        "high" | "xhigh" | "max" => Some("high"),
        _ => Some("high"), // an unknown level asks for more thinking, not less
    }
}

/// Did a TERMINAL failure event report a rate/usage limit?
///
/// Codex has no error kind — `ThreadErrorEvent` is `{message: String}` — so text is all there is.
/// This is exactly what Claude's detector does with its own prose. Gated to the terminal failure
/// events only, so a tool that merely PRINTS "429" cannot trip a false 30-minute backoff.
fn rate_limited(v: &Value, ty: &str) -> bool {
    let text = match ty {
        "turn.failed" => v.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()),
        "error" => v.get("message").and_then(|m| m.as_str()),
        _ => None,
    };
    text.map(super::looks_rate_limited).unwrap_or(false)
}

/// The model's ANSWER from a `--json` stream: the LAST `agent_message` item. Codex emits its
/// reasoning and progress as separate items; the final agent_message is the reply. Falls back to
/// the raw bytes so a schema change degrades to "pass the text through" rather than to an empty
/// verdict (a judge that silently returns nothing would read as "not met").
fn last_agent_message(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let last = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("item.completed"))
        .filter_map(|v| {
            let item = v.get("item")?;
            if item.get("type")?.as_str()? != "agent_message" {
                return None;
            }
            item.get("text")?.as_str().map(str::to_string)
        })
        .next_back();
    last.unwrap_or_else(|| text.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPTURED: &str = include_str!("../../tests/fixtures/agent-streams/codex-0.144.1.jsonl");

    #[test]
    fn usage_comes_from_turn_completed_and_reasoning_is_not_double_counted() {
        let total: u64 = CAPTURED.lines().filter_map(|l| Codex.parse_usage(l)).sum();
        // the captured run reported output_tokens: 151 with reasoning_output_tokens: 16.
        // 151, not 167 — reasoning is a BREAKDOWN of output, not an addition to it.
        assert_eq!(total, 151, "reasoning_output_tokens must NOT be summed on top of output_tokens");
    }

    /// THE trap that terminal-only parsing gets wrong: Codex's resume handle is on the FIRST event.
    #[test]
    fn the_session_id_comes_from_the_first_event_not_the_terminal_one() {
        let id = CAPTURED
            .lines()
            .find_map(|l| Codex.parse_session_id(l))
            .expect("thread.started carries the resume handle");
        assert_eq!(id, "019f5639-83d0-7073-ba55-b56851c99e90");

        // and the TERMINAL event carries no id at all — a terminal-only reader would find nothing.
        let terminal = CAPTURED.lines().find(|l| l.contains("turn.completed")).unwrap();
        assert_eq!(Codex.parse_session_id(terminal), None);
    }

    /// All THREE terminal shapes must be recognised — success, turn failure, and a bare error.
    #[test]
    fn all_three_terminal_shapes_are_terminal() {
        assert!(Codex.parse_result(r#"{"type":"turn.completed","usage":{}}"#).is_some());
        assert!(Codex.parse_result(r#"{"type":"turn.failed","error":{"message":"401"}}"#).is_some());
        assert!(Codex.parse_result(r#"{"type":"error","message":"boom"}"#).is_some());
        assert!(Codex.parse_result(r#"{"type":"turn.started"}"#).is_none());
        // and none of them invents a cost Codex never reported
        let r = Codex.parse_result(r#"{"type":"turn.completed","usage":{}}"#).unwrap();
        assert_eq!(r.cost_usd, None);
    }

    /// The `-p` trap: in `codex exec`, `-p` is `--profile`. The prompt is POSITIONAL and last.
    #[test]
    fn the_prompt_is_positional_never_behind_dash_p() {
        let spec = SessionSpec {
            prompt: "do the thing",
            model: "",  // the default — see DEFAULT_MODEL
            effort: "", // codex has none
            resume_id: None,
            extra_args: &[],
            cwd: Path::new("/tmp"),
        };
        let cmd = Codex.session_command(&spec);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args[0], "exec");
        assert!(!args.contains(&"-p".to_string()), "-p is --profile in codex — NEVER the prompt");
        assert!(!args.contains(&"--effort".to_string()), "codex has no --effort FLAG; it uses -c");
        assert_eq!(args.last().unwrap(), "do the thing", "prompt is positional and LAST");
        // REGRESSION: naming a model default (`gpt-5-codex`) is a hard 400 on a ChatGPT account —
        // the available models depend on how the user authenticated. Empty ⇒ omit the flag.
        assert!(
            !args.contains(&"--model".to_string()),
            "an empty model must OMIT --model, not pass it empty"
        );

        // …but an explicit model IS passed through.
        let cmd = Codex.session_command(&SessionSpec { model: "o3", ..spec });
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let m = args.iter().position(|a| a == "--model").expect("explicit model is passed");
        assert_eq!(args[m + 1], "o3");
    }

    /// Resume RESTRUCTURES argv into a subcommand — it is not a flag.
    #[test]
    fn resume_is_a_subcommand_not_a_flag() {
        let spec = SessionSpec {
            prompt: "carry on",
            model: "m",
            effort: "",
            resume_id: Some("thread-1"),
            extra_args: &[],
            cwd: Path::new("/tmp"),
        };
        let cmd = Codex.session_command(&spec);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[0..3], &["exec", "resume", "thread-1"], "codex exec resume <ID> …");
        assert!(!args.contains(&"--resume".to_string()));
        assert_eq!(args.last().unwrap(), "carry on");
    }

    #[test]
    fn codex_declares_what_it_genuinely_cannot_do() {
        let c = Codex.capabilities();
        assert!(c.reports_output_tokens && c.supports_resume);
        assert!(!c.reports_cost_usd, "codex reports no dollar cost anywhere");
        assert!(c.supports_effort, "via -c model_reasoning_effort= (verified working)");
        assert!(c.supports_one_shot, "read-only sandbox = can host a judge that cannot WRITE");
        assert_eq!(Codex.default_effort(), "high", "codex's ceiling — agg's `max` clamps to it");
        assert!(c.detects_rate_limits, "turn.failed/error carry the text, same as Claude");
        // the level mapping: agg speaks Claude's vocabulary; codex tops out at `high`.
        assert_eq!(effort_arg(""), None, "empty = pass nothing");
        assert_eq!(effort_arg("low"), Some("low"));
        assert_eq!(effort_arg("max"), Some("high"), "clamp, don't reject — `max` = as hard as it thinks");
        assert_eq!(effort_arg("xhigh"), Some("high"));
        // rate limits are read from the TERMINAL failure events only, never tool output.
        let rl = Codex.parse_result(r#"{"type":"turn.failed","error":{"message":"exceeded retry limit, last status: 429 Too Many Requests"}}"#).unwrap();
        assert!(rl.rate_limited, "a 429 on turn.failed must back the loop off");
        let ok = Codex.parse_result(r#"{"type":"turn.completed","usage":{}}"#).unwrap();
        assert!(!ok.rate_limited);
    }
}
