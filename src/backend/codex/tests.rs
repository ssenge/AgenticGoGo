use super::*;

const CAPTURED: &str = include_str!("../../../tests/fixtures/agent-streams/codex-0.144.1.jsonl");

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
        isolation: crate::isolation::Isolation::None,
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
        isolation: crate::isolation::Isolation::None,
    };
    let cmd = Codex.session_command(&spec);
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(&args[0..3], &["exec", "resume", "thread-1"], "codex exec resume <ID> …");
    assert!(!args.contains(&"--resume".to_string()));
    assert_eq!(args.last().unwrap(), "carry on");
}

/// Blast-radius isolation is agent-NATIVE for Codex: it self-sandboxes, so `spec.isolation` picks
/// its flags rather than the OS wrapper. `None` keeps today's bypass; `Sandbox` swaps to
/// `--sandbox workspace-write` and re-enables network (workspace-write denies it by default).
#[test]
fn isolation_selects_codex_native_sandbox_flags() {
    let base = SessionSpec {
        prompt: "do the thing",
        model: "",
        effort: "",
        resume_id: None,
        extra_args: &[],
        cwd: Path::new("/tmp"),
        isolation: crate::isolation::Isolation::None,
    };

    // None ⇒ the auto-mode bypass, exactly as before this feature.
    let none: Vec<String> = Codex
        .session_command(&SessionSpec { isolation: crate::isolation::Isolation::None, ..base })
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        none.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()),
        "isolation: none keeps the bypass"
    );
    assert!(!none.contains(&"--sandbox".to_string()), "none must NOT pass --sandbox");

    // Sandbox ⇒ Codex's own kernel sandbox, writes confined to the workspace, network re-opened.
    let sb: Vec<String> = Codex
        .session_command(&SessionSpec { isolation: crate::isolation::Isolation::Sandbox, ..base })
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let s = sb.iter().position(|a| a == "--sandbox").expect("sandbox tier passes --sandbox");
    assert_eq!(sb[s + 1], "workspace-write", "workspace-write is exactly agg's target policy");
    assert!(
        !sb.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()),
        "the bypass must be GONE once confined"
    );
    // network is denied by workspace-write by default; the owner wants full internet, so re-enable it.
    assert!(
        sb.contains(&"sandbox_workspace_write.network_access=true".to_string()),
        "workspace-write denies net by default — sandbox tier must re-open it (owner wants full internet)"
    );
    // --skip-git-repo-check survives under BOTH tiers (headless runs outside a repo).
    assert!(sb.contains(&"--skip-git-repo-check".to_string()) && none.contains(&"--skip-git-repo-check".to_string()));
}

/// Codex is the ONE backend agg must not wrap in the OS sandbox — it has its own kernel jail.
#[test]
fn codex_self_sandboxes_so_it_is_never_os_wrapped() {
    use crate::backend::AgentBackend;
    assert!(Codex.self_sandboxes(), "Codex has a native kernel sandbox — worker.rs must skip the OS wrapper");
    assert!(Codex.writable_state_paths().is_empty(), "self-sandboxing Codex needs no OS-wrapper write carve-out");
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
