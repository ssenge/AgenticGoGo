//! The GitHub Copilot CLI backend — `copilot -p`.
//!
//! Every field path here was READ OFF THE WIRE, not off a doc. Copilot's SDK documentation
//! describes a `session.shutdown` terminal, an `assistant.usage` event, and no top-level session
//! id. **The CLI emits none of those.** The captured stream that proves it is checked in at
//! `tests/fixtures/agent-streams/copilot-1.0.70.jsonl`, and the parser tests below run against it,
//! so a future version bump that changes the wire fails a test instead of silently mis-reporting.
//!
//! Verified against `GitHub Copilot CLI 1.0.70`:
//!
//! ```text
//!   assistant.reasoning       data.content            the model thinking
//!   assistant.message         data.content            its answer
//!                             data.outputTokens       usage — PER MESSAGE, see parse_usage
//!   tool.execution_start      data.toolName/arguments a tool call
//!   tool.execution_complete   data.success/result     its outcome
//!   result                    sessionId/exitCode/usage TERMINAL (fields are TOP-LEVEL, no `data`)
//! ```

use super::{stream, AgentBackend, Capabilities, OneShot, SessionReport, SessionSpec};
use crate::util::truncate;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};

/// Copilot's default model on the free tier is chosen by `auto`; naming it explicitly keeps the
/// invocation reproducible.
pub const DEFAULT_MODEL: &str = "auto";
/// The summarizer/judge model. Unused today — see `supports_one_shot` below.
pub const DEFAULT_SUMMARY_MODEL: &str = "auto";

/// The isolation a JUDGE/summarizer call runs under. **`--allow-all-tools` is deliberately absent**
/// — that flag belongs to the worker. Without it Copilot denies writes at execution, which is what
/// stops a judge from editing the artifact it is grading. A test asserts this list, so adding an
/// allow-all flag here fails loudly rather than silently handing the judge write access.
const ONE_SHOT_FLAGS: &[&str] = &[
    "--output-format",
    "json",
    "--no-custom-instructions", // don't load repo AGENTS.md — the worker writes those
    "--disable-builtin-mcps",   // no MCP servers (Claude's --strict-mcp-config equivalent)
    "--no-color",
    "--no-auto-update",
];

pub struct Copilot;

impl AgentBackend for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn bin(&self) -> &'static str {
        "copilot"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // data.outputTokens on every assistant.message (summed — see parse_usage).
            reports_output_tokens: true,
            // NO. Copilot bills in GitHub AI Credits; its terminal `usage` object carries
            // `premiumRequests` and durations, and there is no dollar figure anywhere in the
            // stream. `over_cost` is therefore unenforceable — refused at startup, with
            // `spend_ceiling_hint` pointing at Copilot's own ceiling instead.
            reports_cost_usd: false,
            // `result.sessionId` + `-r/--resume=<id>`.
            supports_resume: true,
            // `--effort`, and the same vocabulary Claude uses (low|medium|high|xhigh|max).
            supports_effort: true,
            // NOT VERIFIED, and the docs suggest a rate-limited session may PAUSE AND AUTO-RETRY
            // silently — in which case it never surfaces to us at all. Claiming detection we don't
            // have would make the loop burn its session budget on retries it thinks are failures.
            // False until someone observes a real limit on the wire.
            detects_rate_limits: false,
            // YES — by NOT passing `--allow-all-tools`, so any write is permission-denied.
            //
            // The requirement was never "tools must be absent" — it is "the judge must not be able
            // to MODIFY the artifact it is grading". Probing this, the model DID call the `create`
            // tool and the write was DENIED at execution (`success: false, "Permission denied"`),
            // leaving the file untouched. That is the same mechanism Claude relies on: its one-shot
            // call omits `--dangerously-skip-permissions`, so its built-in tools are denied too
            // (`--strict-mcp-config` only disables MCP servers, not Bash/Edit).
            supports_one_shot: true,
        }
    }

    fn default_model(&self) -> &'static str {
        DEFAULT_MODEL
    }
    fn default_summary_model(&self) -> &'static str {
        DEFAULT_SUMMARY_MODEL
    }

    /// Copilot bills in AI Credits, so it cannot report dollars — but it is NOT unboundable.
    fn spend_ceiling_hint(&self) -> Option<&'static str> {
        Some("Copilot bills in AI Credits, so cap the session with `--max-ai-credits <n>` \
              via agg.yaml `worker_args`")
    }

    /// `copilot -p <prompt> --output-format json` — the prompt goes on `-p` (unlike Codex, where
    /// `-p` means `--profile` and the prompt is positional). stdin is nulled: piped stdin is
    /// ignored when `-p` is given, and a non-TTY child must never block on it.
    fn session_command(&self, spec: &SessionSpec) -> Command {
        let mut command = Command::new(self.bin());
        command
            .arg("--output-format")
            .arg("json")
            // the worker must never block on a permission prompt — it is headless.
            .arg("--allow-all-tools")
            .arg("--no-color")
            // never let the CLI update itself mid-run.
            .arg("--no-auto-update")
            .arg("--model")
            .arg(spec.model);
        if !spec.effort.is_empty() {
            command.arg("--effort").arg(spec.effort);
        }
        if let Some(id) = spec.resume_id {
            command.arg("--resume").arg(id);
        }
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
        let v: Value = serde_json::from_str(line).ok()?;
        let ty = v.get("type")?.as_str()?;
        let data = v.get("data");
        let d = |k: &str| data.and_then(|d| d.get(k));

        match ty {
            "assistant.reasoning" => {
                let text = clean(d("content")?.as_str()?);
                if text.is_empty() {
                    return None;
                }
                Some(stream::Event {
                    display: format!("💬 {}", truncate(&text, 200)),
                    kind: stream::EventKind::Think,
                    text: truncate(&text, 200),
                    is_result: false,
                    thought: Some(text),
                })
            }
            "assistant.message" => {
                let text = clean(d("content")?.as_str()?);
                if text.is_empty() {
                    return None; // empty message envelopes carry only usage — nothing to show
                }
                Some(stream::Event {
                    display: format!("💬 {}", truncate(&text, 200)),
                    kind: stream::EventKind::Think,
                    text: truncate(&text, 200),
                    is_result: false,
                    thought: Some(text),
                })
            }
            "tool.execution_start" => {
                let name = d("toolName").and_then(|x| x.as_str()).unwrap_or("tool");
                // summarise the arguments rather than dumping a whole file body into the log.
                let args = d("arguments").map(summarize_args).unwrap_or_default();
                let text = if args.is_empty() { name.to_string() } else { format!("{name} {args}") };
                Some(stream::Event {
                    display: format!("🔧 {}", truncate(&text, 200)),
                    kind: stream::EventKind::Tool,
                    text: truncate(&text, 200),
                    is_result: false,
                    thought: None,
                })
            }
            "tool.execution_complete" => {
                let ok = d("success").and_then(|x| x.as_bool()).unwrap_or(false);
                let body = if ok {
                    d("result")
                        .and_then(|r| r.get("content"))
                        .and_then(|c| c.as_str())
                        .map(clean)
                        .unwrap_or_default()
                } else {
                    d("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .map(clean)
                        .unwrap_or_else(|| "failed".into())
                };
                let text = format!("{} {}", if ok { "ok" } else { "ERR" }, truncate(&body, 160));
                Some(stream::Event {
                    display: format!("↳ {text}"),
                    kind: stream::EventKind::ToolResult,
                    text,
                    is_result: false,
                    thought: None,
                })
            }
            "result" => {
                let code = v.get("exitCode").and_then(|x| x.as_i64()).unwrap_or(-1);
                let text = format!("RESULT exit={code}");
                Some(stream::Event {
                    display: format!("✅ {text}"),
                    kind: stream::EventKind::Result,
                    text,
                    is_result: true,
                    thought: None,
                })
            }
            _ => None, // session.* / user.message / *_delta / *_start — noise for our purposes
        }
    }

    /// **Per message, not on the terminal event.** `assistant.message.data.outputTokens`. Copilot's
    /// `result` event carries NO token count at all (its `usage` holds `premiumRequests` and
    /// durations), so a terminal-only reader would report ZERO tokens for every Copilot session and
    /// silently disarm the budget guard. The worker SUMS these. See [`AgentBackend::parse_usage`].
    fn parse_usage(&self, line: &str) -> Option<u64> {
        let v: Value = serde_json::from_str(line).ok()?;
        if v.get("type")?.as_str()? != "assistant.message" {
            return None;
        }
        v.get("data")?.get("outputTokens")?.as_u64()
    }

    /// Copilot's terminal event is `result`, and its fields are TOP-LEVEL (no `data` wrapper) —
    /// unlike every other event in the stream. `sessionId` is here, which is what makes resume
    /// possible; `cost_usd` is `None` because Copilot has no dollar figure anywhere.
    fn parse_result(&self, line: &str) -> Option<SessionReport> {
        let v: Value = serde_json::from_str(line).ok()?;
        if v.get("type")?.as_str()? != "result" {
            return None;
        }
        Some(SessionReport {
            cost_usd: None,      // AI Credits, not dollars — see capabilities()
            rate_limited: false, // not detectable today — see capabilities()
        })
    }

    /// On the TERMINAL `result` event, as `sessionId` — and note its fields are TOP-LEVEL, unlike
    /// every other Copilot event, which nests under `data`.
    fn parse_session_id(&self, line: &str) -> Option<String> {
        let v: Value = serde_json::from_str(line).ok()?;
        if v.get("type")?.as_str()? != "result" {
            return None;
        }
        v.get("sessionId")?.as_str().map(str::to_string)
    }

    /// Copilot's `--effort` takes the same vocabulary Claude does, so agg's default carries over.
    fn default_effort(&self) -> &'static str {
        "max"
    }

    /// A judging / summarizing call: the model may READ, but it cannot WRITE.
    ///
    /// # trust boundary
    /// - **No `--allow-all-tools`.** That flag is what the WORKER gets. Without it Copilot's
    ///   default is deny, so a write is refused at execution — verified: the model called `create`
    ///   and got `success: false, "Permission denied"`, and the file was never created.
    /// - **`--no-custom-instructions`** — do NOT load `.github/copilot-instructions.md` / `AGENTS.md`
    ///   from the repo. The worker writes those, so loading them would let it reconfigure its own
    ///   judge. Copilot's equivalent of Claude's `--setting-sources user`.
    /// - **`--disable-builtin-mcps`** — no MCP servers, like Claude's `--strict-mcp-config`.
    fn one_shot(&self, prompt: &str, model: &str, timeout_secs: u64, cwd: Option<&Path>) -> Result<OneShot, String> {
        let mut command = Command::new(self.bin());
        command.args(ONE_SHOT_FLAGS).arg("--model").arg(model).arg("-p").arg(prompt).stdin(Stdio::null());
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        let out = crate::os::proc::run_with_timeout(command, timeout_secs)?;
        Ok(OneShot {
            body: last_assistant_message(&out.stdout),
            stderr: out.stderr,
            success: out.success,
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
                "the GitHub Copilot CLI (`{}`) was not found on your PATH.\n  \
                 Install it with `npm install -g @github/copilot`, then run `copilot login`\n  \
                 (or set GH_TOKEN / COPILOT_GITHUB_TOKEN) and make sure `{} --version` works.",
                self.bin(),
                self.bin()
            );
        }
        Ok(())
    }
}

/// One-line summary of a tool call's arguments — never the full body (a `create` call carries the
/// whole file text, which would flood the log and the dashboard tail).
fn summarize_args(args: &Value) -> String {
    if let Some(obj) = args.as_object() {
        // prefer the fields that identify WHAT is being acted on.
        for k in ["path", "command", "file_path", "query", "url"] {
            if let Some(s) = obj.get(k).and_then(|x| x.as_str()) {
                return clean(s);
            }
        }
    }
    String::new()
}

/// Collapse whitespace so one event is one log line.
fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The model's ANSWER from a `--output-format json` stream: the LAST non-empty
/// `assistant.message.data.content`. (Copilot also emits empty message envelopes that carry only
/// usage — those are not the answer.) Falls back to the raw bytes so a schema change degrades to
/// "pass the text through" rather than to an empty verdict, which a judge would read as "not met".
fn last_assistant_message(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let last = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("assistant.message"))
        .filter_map(|v| {
            let c = v.get("data")?.get("content")?.as_str()?;
            (!c.trim().is_empty()).then(|| c.to_string())
        })
        .next_back();
    last.unwrap_or_else(|| text.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The REAL captured stream (see the module doc). These tests are the reason a Copilot version
    /// bump that changes the wire format fails loudly instead of silently reporting zero tokens.
    const CAPTURED: &str = include_str!("../../tests/fixtures/agent-streams/copilot-1.0.70.jsonl");

    #[test]
    fn tokens_are_summed_from_assistant_messages_not_the_terminal_event() {
        let total: u64 = CAPTURED.lines().filter_map(|l| Copilot.parse_usage(l)).sum();
        assert_eq!(total, 124, "outputTokens ride on assistant.message in the captured stream");

        // and the terminal event contributes NOTHING — this is the whole trap.
        let terminal = CAPTURED.lines().find(|l| l.contains(r#""type":"result""#)).unwrap();
        assert_eq!(
            Copilot.parse_usage(terminal),
            None,
            "copilot's result event carries no token count — a terminal-only reader would see 0"
        );
    }

    #[test]
    fn the_terminal_event_yields_the_session_id_and_no_cost() {
        let terminal = CAPTURED.lines().find(|l| l.contains(r#""type":"result""#)).unwrap();
        let r = Copilot.parse_result(terminal).expect("`result` is the terminal event");
        assert_eq!(r.cost_usd, None, "copilot has no dollar figure anywhere — must be None, not 0.0");
        // Copilot's resume handle IS on the terminal event (unlike Codex, whose is on the first).
        assert_eq!(
            Copilot.parse_session_id(terminal).as_deref(),
            Some("082721a3-5134-4949-855a-9bdabb35cd90")
        );

        // no other line is terminal
        let others = CAPTURED.lines().filter(|l| Copilot.parse_result(l).is_some()).count();
        assert_eq!(others, 1, "exactly one terminal event");
    }

    #[test]
    fn the_assistant_answer_is_surfaced_as_a_thought() {
        let ev = CAPTURED
            .lines()
            .filter_map(|l| Copilot.parse_event(l))
            .find(|e| e.thought.is_some())
            .expect("the model's answer must reach the dashboard");
        assert!(ev.thought.unwrap().contains("OK"), "the captured run answered 'OK'");
    }

    #[test]
    fn the_prompt_is_last_and_stdin_is_never_the_channel() {
        let spec = SessionSpec {
            prompt: "do the thing",
            model: "auto",
            effort: "high",
            resume_id: Some("sess-1"),
            extra_args: &["--max-ai-credits".to_string(), "5".to_string()],
            cwd: Path::new("/tmp"),
        };
        let cmd = Copilot.session_command(&spec);
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(cmd.get_program().to_string_lossy(), "copilot");
        // -p carries the prompt (unlike codex, where -p is --profile)
        let p = args.iter().position(|a| a == "-p").expect("-p present");
        assert_eq!(args[p + 1], "do the thing");
        assert_eq!(args.last().unwrap(), "do the thing", "prompt is last");
        // operator worker_args land before -p so they can extend but not clobber
        let credits = args.iter().position(|a| a == "--max-ai-credits").unwrap();
        assert!(credits < p);
        assert!(args.contains(&"--allow-all-tools".to_string()), "headless must not block on prompts");
        assert!(args.contains(&"--effort".to_string()) && args.contains(&"--resume".to_string()));
    }

    /// A judge call must NOT carry `--allow-all-tools` — that flag is the WORKER's. Without it,
    /// Copilot denies writes at execution, which is what stops a judge editing what it grades.
    /// If this ever regresses, the judge silently gains write access to the repo it is judging.
    #[test]
    fn the_judge_call_never_gets_write_access() {
        assert!(Copilot.capabilities().supports_one_shot);
        // We can't spawn copilot in a unit test, so assert the CONTRACT on the flags we'd send.
        // (The live probe confirmed the behaviour: `create` returned "Permission denied".)
        let forbidden = ["--allow-all-tools", "--allow-all"];
        for f in forbidden {
            assert!(!ONE_SHOT_FLAGS.contains(&f), "a judge must never be given `{f}`");
        }
        for required in ["--no-custom-instructions", "--disable-builtin-mcps"] {
            assert!(ONE_SHOT_FLAGS.contains(&required), "judge isolation needs `{required}`");
        }
    }

    #[test]
    fn copilot_cannot_price_itself_but_says_how_to_cap_itself() {
        let c = Copilot.capabilities();
        assert!(!c.reports_cost_usd, "AI Credits, not dollars");
        assert!(c.reports_output_tokens, "but it DOES report tokens");
        assert!(
            Copilot.spend_ceiling_hint().unwrap().contains("--max-ai-credits"),
            "a refused cost guard must not leave the loop unbounded"
        );
    }
}
