use super::*;

/// The REAL captured stream (see the module doc). These tests are the reason a Copilot version
/// bump that changes the wire format fails loudly instead of silently reporting zero tokens.
const CAPTURED: &str = include_str!("../../../tests/fixtures/agent-streams/copilot-1.0.70.jsonl");

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
        isolation: crate::isolation::Isolation::None,
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

/// Copilot, like Claude, has only an allowlist layer — not a kernel jail. Confinement is the OS
/// wrapper, so its argv must be identical under `none` and `sandbox`: `--allow-all-tools` stays
/// (autonomy) and no `--sandbox` flag appears. It does not self-sandbox, so worker.rs wraps it.
#[test]
fn isolation_does_not_change_copilot_flags_and_it_does_not_self_sandbox() {
    let base = SessionSpec {
        prompt: "p",
        model: "auto",
        effort: "",
        resume_id: None,
        extra_args: &[],
        cwd: Path::new("/tmp"),
        isolation: crate::isolation::Isolation::None,
    };
    let argv = |iso| {
        Copilot
            .session_command(&SessionSpec { isolation: iso, ..base })
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let none = argv(crate::isolation::Isolation::None);
    let sandbox = argv(crate::isolation::Isolation::Sandbox);
    assert_eq!(none, sandbox, "Copilot's flags are identical across isolation tiers — the OS wrapper confines it");
    assert!(none.contains(&"--allow-all-tools".to_string()), "autonomy is kept under BOTH tiers");
    assert!(!none.contains(&"--sandbox".to_string()), "Copilot never grows a --sandbox flag");
    assert!(!Copilot.self_sandboxes(), "Copilot has no kernel jail — worker.rs must wrap it");
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
