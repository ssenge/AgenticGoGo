//! Stream-json event formatting. Turns one raw stream-json line into a readable log
//! line (`🔧 $ <description>`, `🔧 read <path>`, `💬 <thought>`, `✅ RESULT …`),
//! never the raw input JSON soup.
//!
//! # seam
//! This is the **event-parsing half of [`crate::backend`]** — everything here knows Claude's
//! `stream-json` wire format, so it is backend-private in spirit even though it stays a separate
//! module for size. When a second agent backend lands, this moves behind the `trait AgentBackend`
//! together with backend.rs: `backend` builds the invocation, `stream` reads what comes back.

use serde_json::Value;

/// Category of a formatted event — drives the dashboard's per-line coloring and the
/// `now`/`think` split. Mirrors the leading glyph in `display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Tool,       // 🔧
    Think,      // 💬
    ToolResult, // ↳
    Result,     // ✅
    Init,       // ▶
}

impl EventKind {
    /// Stable string tag stored in the serialized dashboard state.
    pub fn tag(self) -> &'static str {
        match self {
            EventKind::Tool => "tool",
            EventKind::Think => "think",
            EventKind::ToolResult => "tool_result",
            EventKind::Result => "result",
            EventKind::Init => "init",
        }
    }
}

/// A formatted stream event.
pub struct Event {
    /// human-readable one-liner for the log (carries the leading glyph)
    pub display: String,
    /// category of the event (for the dashboard tail coloring + now/think split)
    pub kind: EventKind,
    /// `display` with the leading glyph/indent stripped — what the dashboard tail shows
    pub text: String,
    /// true if this is a terminal `result` event (don't count as "activity")
    pub is_result: bool,
    /// assistant thought text, if this event was a `💬` message (drives heartbeat)
    pub thought: Option<String>,
}

/// Tracks rolling activity (the worker's `💬` thoughts) so the LLM summarizer has raw
/// material at the session boundary.
#[derive(Default)]
pub struct ActivityTracker {
    pub thoughts: Vec<String>,
}
impl ActivityTracker {
    pub fn observe(&mut self, ev: &Event) {
        if let Some(t) = &ev.thought {
            self.thoughts.push(t.clone());
        }
    }
}

/// Format one raw stream-json line. Returns `None` for lines that carry no
/// display-worthy content (e.g. unknown event types).
pub fn format_event(line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line).ok()?;
    let ty = v.get("type")?.as_str()?;

    match ty {
        "assistant" => {
            // first text or tool_use block in the message
            let content = v.get("message")?.get("content")?.as_array()?;
            for block in content {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        let clean = clean(text);
                        let shown = truncate(&clean, 220);
                        return Some(Event {
                            display: format!("💬 {shown}"),
                            kind: EventKind::Think,
                            text: shown,
                            is_result: false,
                            thought: Some(clean),
                        });
                    }
                    Some("tool_use") => {
                        let label = tool_label(block);
                        return Some(Event {
                            display: format!("🔧 {label}"),
                            kind: EventKind::Tool,
                            text: label,
                            is_result: false,
                            thought: None,
                        });
                    }
                    _ => {}
                }
            }
            None
        }
        "user" => {
            // tool_result -> a short "↳" line (not activity-worthy beyond display)
            let content = v.get("message")?.get("content")?.as_array()?;
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    let body = truncate(&clean(&tool_result_text(block)), 160);
                    return Some(Event {
                        display: format!("   ↳ {body}"),
                        kind: EventKind::ToolResult,
                        text: body,
                        is_result: false,
                        thought: None,
                    });
                }
            }
            None
        }
        "result" => {
            let dur = v.get("duration_ms").and_then(|x| x.as_u64()).unwrap_or(0);
            let turns = v.get("num_turns").and_then(|x| x.as_u64()).unwrap_or(0);
            let result = v.get("result").and_then(|x| x.as_str()).unwrap_or("");
            let body = format!("RESULT ({dur}ms, {turns} turns): {}", truncate(&clean(result), 300));
            Some(Event {
                display: format!("✅ {body}"),
                kind: EventKind::Result,
                text: body,
                is_result: true,
                thought: None,
            })
        }
        "system" => {
            if v.get("subtype").and_then(|s| s.as_str()) == Some("init") {
                let model = v.get("model").and_then(|m| m.as_str()).unwrap_or("?");
                let body = format!("session init (model {model})");
                Some(Event {
                    display: format!("▶ {body}"),
                    kind: EventKind::Init,
                    text: body,
                    is_result: false,
                    thought: None,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Readable label for a tool_use block — pick the human field per tool, NEVER the
/// raw input JSON (which renders as unreadable mid-quote "echo..." soup).
fn tool_label(block: &Value) -> String {
    let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
    let input = block.get("input").cloned().unwrap_or(Value::Null);
    let get = |k: &str| input.get(k).and_then(|x| x.as_str()).map(str::to_string);

    match name {
        "Bash" => {
            let d = get("description").or_else(|| get("command")).unwrap_or_default();
            format!("$ {}", truncate(&clean(&d), 90))
        }
        "Read" => format!("read {}", get("file_path").unwrap_or_else(|| "?".into())),
        "Edit" => format!("edit {}", get("file_path").unwrap_or_else(|| "?".into())),
        "Write" => format!("write {}", get("file_path").unwrap_or_else(|| "?".into())),
        "NotebookEdit" => format!("edit {}", get("notebook_path").unwrap_or_else(|| "?".into())),
        "Grep" => format!("grep {}", truncate(&get("pattern").unwrap_or_default(), 60)),
        "Glob" => format!("glob {}", truncate(&get("pattern").unwrap_or_default(), 60)),
        "Task" | "Agent" => format!("agent: {}", truncate(&get("description").unwrap_or_default(), 80)),
        n if n.starts_with("mcp__") => {
            // bare tool name, no args: mcp__plugin__tool -> tool
            n.rsplit("__").next().unwrap_or(n).to_string()
        }
        n => format!("{n} {}", truncate(&get("description").unwrap_or_default(), 60)),
    }
}

/// Extract a short text body from a tool_result block (its content may be a
/// string or an array of {type:text,text}).
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// True if a stream-json line is a terminal `result` event reporting a real
/// rate-limit / usage-limit error. GATE: only the terminal result event is scanned, with
/// tight API-error patterns (not prose) — so a tool_result that merely mentions "429" or
/// "rate_limit_error" in passing never trips a false backoff.
pub fn line_is_rate_limited_result(line: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
    if v.get("type").and_then(|t| t.as_str()) != Some("result") {
        return false;
    }
    let mut hay = String::new();
    for k in ["result", "error", "subtype"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            hay.push_str(s);
            hay.push(' ');
        }
    }
    let h = hay.to_lowercase();
    const PATS: &[&str] = &[
        "rate_limit_error",
        "usage limit reached",
        "status 429",
        "http 429",
        "overloaded_error",
        "too many requests",
    ];
    PATS.iter().any(|p| h.contains(p))
}

/// Extract the `session_id` from a terminal `result` event (for `--resume`), if any.
pub fn session_id_from_result(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "result" {
        return None;
    }
    v.get("session_id")?.as_str().map(str::to_string)
}

/// Extract output-token usage from a terminal `result` event, if present.
/// `claude --output-format stream-json` reports a `usage` object on the result;
/// we sum the output-side tokens (the meaningful cost driver for the budget).
/// Returns 0 if the line is not a result or carries no usage.
pub fn output_tokens_from_result(line: &str) -> u64 {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return 0 };
    if v.get("type").and_then(|t| t.as_str()) != Some("result") {
        return 0;
    }
    let usage = match v.get("usage") {
        Some(u) => u,
        None => return 0,
    };
    let get = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    // output tokens + cache-creation (also output-priced); ignore input/read.
    get("output_tokens") + get("cache_creation_input_tokens")
}

/// Extract the session's dollar cost from a terminal `result` event, if present.
/// Claude computes the price itself and reports `total_cost_usd` on the result — correctly
/// per-model (including the `[1m]` variant), cache-aware, no pricing table needed on our side.
/// We just read it. Returns 0.0 if the line is not a result or carries no cost.
pub fn cost_usd_from_result(line: &str) -> f64 {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return 0.0 };
    if v.get("type").and_then(|t| t.as_str()) != Some("result") {
        return 0.0;
    }
    v.get("total_cost_usd").and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn clean(s: &str) -> String {
    // collapse newlines/tabs/runs of spaces to single spaces
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        let c = if ch == '\n' || ch == '\t' || ch == '\r' { ' ' } else { ch };
        if c == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

use crate::util::truncate;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_shows_description_not_soup() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"head -3 /tmp/x; echo \"---\"","description":"Probe x"}}]}}"#;
        let ev = format_event(line).unwrap();
        assert_eq!(ev.display, "🔧 $ Probe x");
    }

    #[test]
    fn bash_falls_back_to_command_when_no_description() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"grep -n foo bar"}}]}}"#;
        let ev = format_event(line).unwrap();
        assert_eq!(ev.display, "🔧 $ grep -n foo bar");
    }

    #[test]
    fn read_shows_path() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/b.rs","offset":1}}]}}"#;
        let ev = format_event(line).unwrap();
        assert_eq!(ev.display, "🔧 read /a/b.rs");
    }

    #[test]
    fn mcp_tool_is_bare_name() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__engram__mem_search","input":{"query":"x"}}]}}"#;
        let ev = format_event(line).unwrap();
        assert_eq!(ev.display, "🔧 mem_search");
    }

    #[test]
    fn text_becomes_thought() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Now I will\nimplement the parser"}]}}"#;
        let ev = format_event(line).unwrap();
        assert_eq!(ev.display, "💬 Now I will implement the parser");
        assert_eq!(ev.thought.as_deref(), Some("Now I will implement the parser"));
    }

    #[test]
    fn session_id_extracted_from_result_only() {
        let result = r#"{"type":"result","session_id":"abc-123","result":"x"}"#;
        assert_eq!(session_id_from_result(result).as_deref(), Some("abc-123"));
        // a non-result line with a session_id field must NOT be picked up here
        let other = r#"{"type":"system","session_id":"sys-1"}"#;
        assert_eq!(session_id_from_result(other), None);
    }

    #[test]
    fn result_is_flagged() {
        let line = r#"{"type":"result","subtype":"success","duration_ms":1000,"num_turns":5,"result":"done"}"#;
        let ev = format_event(line).unwrap();
        assert!(ev.is_result);
        assert!(ev.display.starts_with("✅ RESULT"));
    }

    #[test]
    fn cost_extracted_from_result_only() {
        // a terminal result with total_cost_usd → that float
        let result = r#"{"type":"result","subtype":"success","total_cost_usd":0.246815,"result":"done"}"#;
        assert_eq!(cost_usd_from_result(result), 0.246815);
        // a non-result line (even with the field) → 0.0
        let other = r#"{"type":"assistant","total_cost_usd":9.99,"message":{"content":[]}}"#;
        assert_eq!(cost_usd_from_result(other), 0.0);
        // a result with no cost field → 0.0 (not a panic)
        let no_cost = r#"{"type":"result","subtype":"success","result":"done"}"#;
        assert_eq!(cost_usd_from_result(no_cost), 0.0);
        // garbage → 0.0
        assert_eq!(cost_usd_from_result("not json"), 0.0);
    }

    #[test]
    fn rate_limit_only_on_terminal_result() {
        // a tool_result that merely CONTAINS the words must NOT trigger
        let benign = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"the code mentions rate_limit_error and 429"}]}}"#;
        assert!(!line_is_rate_limited_result(benign));
        // a real terminal result error DOES
        let real = r#"{"type":"result","subtype":"error","error":"API rate_limit_error (status 429)","result":""}"#;
        assert!(line_is_rate_limited_result(real));
    }
}
