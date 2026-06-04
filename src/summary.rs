//! LLM summaries (Phase 3) — condense a session's worker thoughts + the cycle's
//! goal deltas into two human one-liners:
//!   - **cumulative**: the story so far (fed the previous cumulative summary), and
//!   - **windowed**: just this session/window, independent.
//!
//! One cheap Claude call per cycle returns BOTH (as JSON) to minimize cost. The
//! call is auth-safe (NOT `--bare`, which breaks login; `--strict-mcp-config` to
//! stay lean) — same lesson as the LLM judge in Phase 2.

use crate::engine::GoalDelta;
use serde::Deserialize;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The two summary lines produced per cycle.
#[derive(Debug, Clone, Default)]
pub struct Summaries {
    pub cumulative: String,
    pub windowed: String,
}

#[derive(Deserialize)]
struct RawSummaries {
    cumulative: String,
    windowed: String,
}

/// Build the summarizer prompt and call the model. `prev_cumulative` is the last
/// cumulative summary (empty on the first cycle). Returns `None` on any failure —
/// summaries are best-effort and must never break the loop.
pub fn summarize(
    model: &str,
    prev_cumulative: &str,
    thoughts: &[String],
    deltas: &[GoalDelta],
    timeout_secs: u64,
) -> Option<Summaries> {
    // keep input small + cheap: last ~30 thoughts, only changed deltas.
    let recent: Vec<&String> = thoughts.iter().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
    let thoughts_block = if recent.is_empty() {
        "(no thoughts captured this session)".to_string()
    } else {
        recent.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n")
    };
    let changed: Vec<String> = deltas.iter().filter(|d| d.changed()).map(|d| d.line()).collect();
    let deltas_block = if changed.is_empty() {
        "(no goal changed this cycle)".to_string()
    } else {
        changed.iter().map(|d| format!("- {d}")).collect::<Vec<_>>().join("\n")
    };
    let prev_block = if prev_cumulative.trim().is_empty() {
        "(this is the first cycle — no prior summary)".to_string()
    } else {
        prev_cumulative.to_string()
    };

    let prompt = format!(
        "You are a progress summarizer for an autonomous coding agent loop. Be concise, \
         concrete, and factual — no fluff.\n\n\
         PREVIOUS CUMULATIVE SUMMARY (the story so far):\n{prev_block}\n\n\
         WORKER THOUGHTS THIS SESSION (newest last):\n{thoughts_block}\n\n\
         GOAL CHANGES THIS CYCLE:\n{deltas_block}\n\n\
         Produce TWO one-sentence summaries:\n\
         1. \"cumulative\": update the previous cumulative summary with this session's \
         progress — the overall arc (what's being built, where it stands, what's blocking).\n\
         2. \"windowed\": ONLY what happened in this session/cycle, independent of history.\n\n\
         Each must mention concrete goal progress when a goal changed. Output ONLY this JSON \
         on the last line, nothing after it:\n\
         {{\"cumulative\": \"<one sentence>\", \"windowed\": \"<one sentence>\"}}"
    );

    let mut command = Command::new("claude");
    command
        .arg("-p")
        .arg(&prompt)
        .arg("--model")
        .arg(model)
        .arg("--output-format")
        .arg("json")
        .arg("--strict-mcp-config")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let out = run_with_timeout(command, timeout_secs)?;
    // unwrap the claude json envelope -> the model's text
    let body = serde_json::from_slice::<serde_json::Value>(&out)
        .ok()
        .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(&out).into_owned());

    let raw = parse_summaries(&body)?;
    Some(Summaries { cumulative: raw.cumulative, windowed: raw.windowed })
}

/// Extract the summaries JSON (tolerant: handles ```json fences / trailing prose).
fn parse_summaries(text: &str) -> Option<RawSummaries> {
    let trimmed = text.trim();
    if let Ok(r) = serde_json::from_str::<RawSummaries>(trimmed) {
        return Some(r);
    }
    let block = last_json_object(trimmed)?;
    serde_json::from_str::<RawSummaries>(block).ok()
}

fn last_json_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let end = (0..bytes.len()).rev().find(|&i| bytes[i] == b'}')?;
    let mut depth = 0i32;
    for i in (0..=end).rev() {
        match bytes[i] {
            b'}' => depth += 1,
            b'{' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[i..=end]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Run a command with a wall-clock timeout; return stdout bytes, or None on any failure.
/// stdout is drained on a background thread while waiting, so a large JSON envelope
/// (the claude `--output-format json` result) can't fill the pipe and force a timeout.
fn run_with_timeout(mut command: Command, timeout_secs: u64) -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().ok()?;
    let pid = child.id();
    let mut out_pipe = child.stdout.take();
    let out_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = std::io::Read::read_to_end(p, &mut buf);
        }
        buf
    });
    // also drain stderr so it can't fill and block the child
    let mut err_pipe = child.stderr.take();
    let err_h = std::thread::spawn(move || {
        if let Some(p) = err_pipe.as_mut() {
            let mut sink = Vec::new();
            let _ = std::io::Read::read_to_end(p, &mut sink);
        }
    });

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_group(pid);
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
    let _ = err_h.join();
    out_h.join().ok()
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }
    unsafe {
        libc_kill(-(pid as i32), 9);
    }
}
#[cfg(not(unix))]
fn kill_group(pid: u32) {
    let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let s = r#"{"cumulative":"still building the parser","windowed":"fixed an edge case"}"#;
        let r = parse_summaries(s).unwrap();
        assert_eq!(r.cumulative, "still building the parser");
        assert_eq!(r.windowed, "fixed an edge case");
    }

    #[test]
    fn parses_through_fence_and_prose() {
        let s = "Here are the summaries:\n```json\n{\"cumulative\":\"a\",\"windowed\":\"b\"}\n```";
        let r = parse_summaries(s).unwrap();
        assert_eq!(r.cumulative, "a");
        assert_eq!(r.windowed, "b");
    }

    #[test]
    fn none_on_garbage() {
        assert!(parse_summaries("no json here").is_none());
    }

    // Real end-to-end test — hits a live haiku call. Ignored by default so the
    // normal suite stays offline/fast. Run with: cargo test -- --ignored real_summary
    #[test]
    #[ignore]
    fn real_summary() {
        use crate::engine::GoalDelta;
        use crate::model::Lifecycle;
        let thoughts = vec![
            "Reading the parser module to understand the token grammar.".to_string(),
            "Found a panic in parse_expr on nested groups — debugging the recursion.".to_string(),
            "Fixed it: the depth counter was off by one. The test suite passes now.".to_string(),
        ];
        let deltas = vec![GoalDelta {
            id: "tests_pass".into(),
            before_value: 17.0,
            after_value: 18.0,
            before_state: Lifecycle::InProgress,
            after_state: Lifecycle::InProgress,
            rationale: "the nested-group case now passes".into(),
        }];
        let s = summarize("haiku", "Building the expression parser; not all tests passing yet.", &thoughts, &deltas, 120)
            .expect("summarizer returned None (real claude call failed?)");
        println!("\nCUMULATIVE: {}\nWINDOWED:   {}\n", s.cumulative, s.windowed);
        assert!(!s.cumulative.is_empty());
        assert!(!s.windowed.is_empty());
    }
}
