//! Judge execution. A judge yields a [`Verdict`].
//!
//! - `script` judge (Phase 1): run a command, parse its stdout as verdict JSON.
//! - `llm` judge (Phase 2): build a prompt from a rubric + inputs, call
//!   `claude -p --bare --model <m> --output-format json`, extract the verdict
//!   JSON from the model's result. `--bare` = no plugins/hooks/CLAUDE.md → fast,
//!   cheap, deterministic.
//!
//! Both kinds are crash-safe: any failure (spawn, timeout, malformed output)
//! yields `Verdict::failed(...)` rather than panicking.

use crate::model::{JudgeSpec, Verdict};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Run the judge for a goal and return its verdict.
pub fn run(spec: &JudgeSpec, cwd: &Path) -> Verdict {
    match spec {
        JudgeSpec::Script { cmd, timeout } => run_script(cmd, *timeout, cwd),
        JudgeSpec::Llm { model, rubric, inputs, timeout } => {
            run_llm(model, rubric, inputs, *timeout, cwd)
        }
    }
}

// ---------------- script judge ----------------

fn run_script(cmd: &str, timeout_secs: u64, cwd: &Path) -> Verdict {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd);
    match run_with_timeout(command, timeout_secs, cwd) {
        Ok(out) => parse_judge_output(&out.stdout, &out.stderr),
        Err(e) => Verdict::failed(e),
    }
}

// ---------------- llm judge ----------------

fn run_llm(model: &str, rubric: &str, inputs: &[String], timeout_secs: u64, cwd: &Path) -> Verdict {
    // 1) read the rubric (the judge's prompt body)
    let rubric_path = cwd.join(rubric);
    let rubric_text = match std::fs::read_to_string(&rubric_path) {
        Ok(t) => t,
        Err(e) => return Verdict::failed(format!("reading rubric {}: {e}", rubric_path.display())),
    };

    // 2) gather inputs into a context block
    let context = gather_inputs(inputs, cwd);

    // 3) assemble the prompt: rubric + context + a hard verdict-format instruction
    let prompt = format!(
        "{rubric}\n\n\
         ===== CONTEXT (artifacts to evaluate) =====\n{context}\n\
         ===== END CONTEXT =====\n\n\
         Now apply the rubric above. Output ONLY a single JSON object on the last line, \
         exactly this shape (no prose after it):\n\
         {{\"met\": <true|false>, \"value\": <number>, \"max\": <number>, \"target\": <number>, \"rationale\": \"<one sentence>\"}}",
        rubric = rubric_text,
        context = context,
    );

    // 4) call claude headless with json output.
    //
    // NOTE (verified Phase 2): we deliberately do NOT pass `--bare`. `--bare` skips
    // keychain reads, so the judge call fails with "Not logged in" — it cannot
    // authenticate. We instead keep a normal headless call (which authenticates) and
    // isolate it with --strict-mcp-config (+ no --mcp-config) so NO MCP servers load,
    // recovering most of --bare's "lean & deterministic" benefit without breaking auth.
    let mut command = Command::new("claude");
    command
        .arg("-p")
        .arg(&prompt)
        .arg("--model")
        .arg(model)
        .arg("--output-format")
        .arg("json")
        .arg("--strict-mcp-config") // judge runs with no MCP servers
        .stdin(Stdio::null());

    let out = match run_with_timeout(command, timeout_secs, cwd) {
        Ok(o) => o,
        Err(e) => return Verdict::failed(format!("llm judge: {e}")),
    };

    // 5) claude --output-format json wraps the answer; the model's text is in `.result`.
    let body = match serde_json::from_slice::<serde_json::Value>(&out.stdout) {
        Ok(v) => v
            .get("result")
            .and_then(|r| r.as_str())
            .map(str::to_string)
            // fall back to raw stdout if the envelope shape is unexpected
            .unwrap_or_else(|| String::from_utf8_lossy(&out.stdout).into_owned()),
        Err(_) => String::from_utf8_lossy(&out.stdout).into_owned(),
    };

    parse_judge_output(body.as_bytes(), &out.stderr)
}

/// Resolve the `inputs` list into a single text context block. Each input is
/// either a special token or a file path (relative to cwd):
///   "diff"        -> `git diff` (working tree)
///   "diff:HEAD~1" -> `git diff HEAD~1`
///   "status"      -> `git status --short`
///   "log:<path>"  -> last 200 lines of <path>
///   "<path>"      -> full contents of <path>
fn gather_inputs(inputs: &[String], cwd: &Path) -> String {
    let mut out = String::new();
    for inp in inputs {
        let (label, body) = resolve_input(inp, cwd);
        out.push_str(&format!("\n--- {label} ---\n{body}\n"));
    }
    if out.is_empty() {
        out.push_str("(no inputs specified)\n");
    }
    out
}

fn resolve_input(inp: &str, cwd: &Path) -> (String, String) {
    if inp == "diff" {
        return ("git diff".into(), git(&["diff"], cwd));
    }
    if let Some(rev) = inp.strip_prefix("diff:") {
        return (format!("git diff {rev}"), git(&["diff", rev], cwd));
    }
    if inp == "status" {
        return ("git status".into(), git(&["status", "--short"], cwd));
    }
    if let Some(path) = inp.strip_prefix("log:") {
        let full = read_file(cwd.join(path));
        let tail: String = full.lines().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        return (format!("tail {path}"), tail);
    }
    // plain file path
    (inp.to_string(), read_file(cwd.join(inp)))
}

fn git(args: &[&str], cwd: &Path) -> String {
    match Command::new("git").args(args).current_dir(cwd).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => format!("(git {} failed: {e})", args.join(" ")),
    }
}

fn read_file(path: std::path::PathBuf) -> String {
    std::fs::read_to_string(&path).unwrap_or_else(|e| format!("(could not read {}: {e})", path.display()))
}

// ---------------- shared: timeout-aware command runner ----------------

struct CmdOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Spawn `command` in `cwd` with piped stdout/stderr and a wall-clock timeout.
/// Returns Err(reason) on spawn failure or timeout (the child is killed).
///
/// stdout/stderr are drained on background threads WHILE we wait, so a judge that
/// emits more than the OS pipe buffer (~64KB) doesn't block on write and get
/// false-killed at the timeout. On timeout the whole process group is SIGKILLed so
/// a judge that shelled out doesn't leave orphan grandchildren running.
fn run_with_timeout(mut command: Command, timeout_secs: u64, cwd: &Path) -> Result<CmdOutput, String> {
    command.current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0); // own group so we can kill the whole tree on timeout
    }
    let mut child = command.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();

    // drain both pipes concurrently so the child never blocks on a full pipe
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = std::io::Read::read_to_end(p, &mut buf);
        }
        buf
    });
    let err_h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = std::io::Read::read_to_end(p, &mut buf);
        }
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_group(pid);
                    let _ = child.wait();
                    return Err(format!("timed out after {timeout_secs}s"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("wait: {e}")),
        }
    }
    // child exited; the drain threads finish as the pipes hit EOF
    let stdout = out_h.join().unwrap_or_default();
    let stderr = err_h.join().unwrap_or_default();
    Ok(CmdOutput { stdout, stderr })
}

/// SIGKILL a process group (negative pid on unix), so a timed-out judge that
/// shelled out takes its grandchildren with it.
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

// ---------------- shared: verdict parsing ----------------

/// Parse judge output bytes into a Verdict; on failure produce a failed verdict
/// that includes a stderr tail for debugging.
fn parse_judge_output(stdout: &[u8], stderr: &[u8]) -> Verdict {
    let s = String::from_utf8_lossy(stdout);
    match parse_verdict(&s) {
        Ok(v) => v,
        Err(e) => {
            let err = String::from_utf8_lossy(stderr);
            let tail: String = err.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
            Verdict::failed(format!("bad verdict JSON ({e}); stderr: {tail}"))
        }
    }
}

/// Extract the verdict JSON from text. Tolerant: takes the whole trimmed output
/// if it parses, else the last balanced `{...}` block.
fn parse_verdict(text: &str) -> Result<Verdict, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("empty output".into());
    }
    if let Ok(v) = serde_json::from_str::<Verdict>(trimmed) {
        return Ok(v);
    }
    if let Some(block) = last_json_object(trimmed) {
        return serde_json::from_str::<Verdict>(block).map_err(|e| e.to_string());
    }
    Err("no JSON object found".into())
}

/// Return the last top-level `{...}` substring (brace-balanced), if any.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn script_judge_parses_clean_json() {
        let spec = JudgeSpec::Script {
            cmd: r#"echo '{"met":true,"value":28,"max":28,"target":28}'"#.into(),
            timeout: 10,
        };
        let v = run(&spec, Path::new("."));
        assert!(v.met);
        assert_eq!(v.value, 28.0);
        assert!(v.error.is_none());
    }

    #[test]
    fn large_output_judge_does_not_false_timeout() {
        // emit ~200KB of log noise (well past the ~64KB pipe buffer) BEFORE the verdict.
        // Without concurrent pipe draining this would block the child and hit the timeout.
        let spec = JudgeSpec::Script {
            cmd: r#"for i in $(seq 1 4000); do echo "noisy log line padding padding padding padding $i"; done; echo '{"met":true,"value":1,"max":1,"target":1}'"#.into(),
            timeout: 15,
        };
        let v = run(&spec, Path::new("."));
        assert!(v.met, "verdict should parse despite huge preceding output; got {:?}", v.error);
        assert!(v.error.is_none());
    }

    #[test]
    fn script_judge_tolerates_log_lines_before_json() {
        let spec = JudgeSpec::Script {
            cmd: r#"echo 'building...'; echo 'done'; echo '{"met":false,"value":18,"max":28}'"#.into(),
            timeout: 10,
        };
        let v = run(&spec, Path::new("."));
        assert!(!v.met);
        assert_eq!(v.value, 18.0);
    }

    #[test]
    fn malformed_judge_yields_failed_verdict() {
        let spec = JudgeSpec::Script { cmd: "echo not-json".into(), timeout: 10 };
        let v = run(&spec, Path::new("."));
        assert!(!v.met);
        assert!(v.error.is_some());
    }

    #[test]
    fn script_judge_times_out() {
        let spec = JudgeSpec::Script { cmd: "sleep 5".into(), timeout: 1 };
        let v = run(&spec, Path::new("."));
        assert!(v.error.as_deref().unwrap().contains("timed out"));
    }

    #[test]
    fn verdict_extracted_from_model_result_envelope() {
        // simulate the claude --output-format json envelope: result holds the text,
        // which ends in the verdict JSON after some prose.
        let envelope = r#"{"type":"result","result":"I assessed the code.\nLooks idiomatic.\n{\"met\":true,\"value\":90,\"max\":100,\"target\":80,\"rationale\":\"clean\"}"}"#;
        let inner = serde_json::from_str::<serde_json::Value>(envelope)
            .unwrap()
            .get("result")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let v = parse_verdict(&inner).unwrap();
        assert!(v.met);
        assert_eq!(v.value, 90.0);
        assert_eq!(v.rationale, "clean");
    }

    #[test]
    fn last_json_object_picks_trailing_block() {
        let s = "prose {not: valid} more\nthen {\"met\":false,\"value\":1}";
        assert_eq!(last_json_object(s), Some(r#"{"met":false,"value":1}"#));
    }
}
