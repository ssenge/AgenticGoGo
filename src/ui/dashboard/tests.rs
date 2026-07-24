//! Dashboard rendering tests (moved out of mod.rs to keep the app module slim).

use super::draw; // draw::draw(...)
use super::fmt::*; // grade_color, human, wrap_indent, …
use super::*;
use crate::state::{ActivityEvent, AgentUsage, DashboardState, JudgeView};
use ratatui::backend::TestBackend;

fn demo_state() -> DashboardState {
    let mut per_agent = std::collections::BTreeMap::new();
    per_agent.insert("claude".to_string(), AgentUsage { tokens: 1_500_000, cost: Some(1.10) });
    per_agent.insert("codex".to_string(), AgentUsage { tokens: 600_000, cost: Some(0.15) });
    DashboardState {
        project: "myproject".into(),
        model: "claude-opus-4-8".into(),
        step: "worker".into(),
        step_agent: "codex".into(),
        step_model: "gpt-5-codex".into(),
        stop_when: "all_goals".into(),
        halt_when: "regressed".into(),
        started_at_epoch: 1_700_000_000,
        up_secs: 11520,
        session: 7,
        lifetime_session: 7,
        phase: Phase::Run,
        idle_secs: 12,
        tokens_spent: 2_100_000,
        budget_total: Some(5_000_000),
        cost_spent: 1.25,
        cost_limit: Some(5.0),
        memory_bytes: 2048,
        goals_met: 1,
        goals_total: 2,
        goals: Vec::new(), // the successor `judges` below drives every reader now.
        per_agent,
        judges: vec![
            JudgeView {
                name: "tests_pass".into(),
                kind: "script".into(),
                in_dod: true,
                invariant: false,
                state: "in_progress".into(),
                met: false,
                value: Some(18.0),
                max: Some(28.0),
                target: 28.0,
                delta: 3.0,
                rationale: "18/28 tests passing; the remaining ten exercise the BOUND-limited \
                            instances that need the conflict-analysis bound-climb lever to land."
                    .into(),
                error: None,
            },
            JudgeView {
                name: "no_regressions".into(),
                kind: "script".into(),
                in_dod: true,
                invariant: true,
                state: "met".into(),
                met: true,
                value: None,
                max: None,
                target: 1.0,
                delta: 0.0,
                rationale: "build green".into(),
                error: None,
            },
            JudgeView {
                name: "stalled".into(),
                kind: "script".into(),
                in_dod: false,
                invariant: false,
                state: "pending".into(),
                met: false,
                value: None,
                max: None,
                target: 1.0,
                delta: 0.0,
                rationale: "state file changed this session — not stalled".into(),
                error: None,
            },
        ],
        now: "🔧 $ Run the test suite".into(),
        think: "implementing the remaining parser cases".into(),
        recent: vec![
            ActivityEvent { ts: "14:07:01".into(), kind: "tool".into(), text: "$ Run the test suite".into() },
            ActivityEvent { ts: "14:07:03".into(), kind: "tool_result".into(), text: "18 passed, 10 failed".into() },
            ActivityEvent { ts: "14:07:05".into(), kind: "think".into(), text: "the bound improved on aflow30a".into() },
        ],
        summary_cumulative: "Building the parser; 18/28 tests pass, no regressions.".into(),
        summary_windowed: "Fixed an edge case; the suite is greener.".into(),
        seq: 5,
        finished: false,
        finish_reason: String::new(),
    }
}

fn render(s: &DashboardState, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    let mut ui = DashboardUi::default();
    term.draw(|f| draw::draw(f, Path::new("."), Some(s), &mut ui)).unwrap();
    let buf = term.backend().buffer().clone();
    buf.content().iter().map(|c| c.symbol()).collect()
}

/// Render the full dashboard headless and assert the key content appears across
/// every panel — proves draw() doesn't panic and lays out the right text.
#[test]
fn renders_all_panels() {
    let s = demo_state();
    let text = render(&s, 100, 40);
    assert!(text.contains("AgenticGoGo")); // title (waiting screen reuses it too)
    assert!(text.contains("Info"));
    assert!(text.contains("myproject"));
    assert!(text.contains("#7"));            // session number
    // the current step + its agent/model live in Info (§7.4).
    assert!(text.contains("worker"));        // step name
    assert!(text.contains("gpt-5-codex"));   // step model (a mixed-run codex step)
    assert!(text.contains("Progress"));
    assert!(text.contains("1/2"));           // DoD aggregate label
    assert!(text.contains("Per-agent"));     // §7.4 per-agent panel
    assert!(text.contains("claude"));        // a per-agent row
    assert!(text.contains("codex"));         // the other agent / the step agent
    assert!(text.contains("Judges"));        // scoreboard title
    assert!(text.contains("tests_pass"));
    assert!(text.contains("18/28"));         // numeric judge shows value/target
    assert!(text.contains("Activity"));
    assert!(text.contains("14:07:05"));      // activity tail timestamp
    assert!(text.contains("bound improved")); // activity tail text
    assert!(text.contains("Summary"));
    assert!(text.contains("Building the parser")); // cumulative summary
}

/// The §7.4 defect this migration fixes, at the TUI: a binary judge renders met/unmet, NOT a
/// fabricated `0`; a run-set judge is shown under its own divider.
#[test]
fn binary_judge_reads_met_and_run_set_is_divided() {
    let s = demo_state();
    let text = render(&s, 100, 44);
    assert!(text.contains("no_regressions"));
    assert!(text.contains("met")); // the binary invariant reads "met", never "0/0"
    assert!(!text.contains("0/0"), "a binary judge must never render a fabricated 0/0");
    assert!(text.contains("run-set")); // the divider before `stalled`
    assert!(text.contains("stalled"));
}

/// The title row must carry the name ONLY — project/model/stop must NOT bleed into
/// it. Row 0 is the centered "AgenticGoGo" banner; row 1 is the Info block border.
#[test]
fn title_is_name_only() {
    let backend = TestBackend::new(100, 40);
    let mut term = Terminal::new(backend).unwrap();
    let s = demo_state();
    let mut ui = DashboardUi::default();
    term.draw(|f| draw::draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
    let buf = term.backend().buffer().clone();
    let row0: String = (0..100).map(|x| buf[(x, 0)].symbol().to_string()).collect();
    assert!(row0.contains("AgenticGoGo"));
    assert!(!row0.contains("myproject")); // details live in Info, not the title
    assert!(!row0.contains("all_goals"));
    // row 1 is the Info block's top border.
    let row1: String = (0..100).map(|x| buf[(x, 1)].symbol().to_string()).collect();
    assert!(row1.contains("Info"));
}

#[test]
fn missing_state_shows_waiting() {
    let backend = TestBackend::new(80, 20);
    let mut term = Terminal::new(backend).unwrap();
    let mut ui = DashboardUi::default();
    term.draw(|f| draw::draw(f, Path::new("/nonexistent"), None, &mut ui)).unwrap();
    let buf = term.backend().buffer().clone();
    let text: String = buf.content().iter().map(|c| c.symbol()).collect();
    assert!(text.contains("waiting for"));
}

#[test]
fn finished_banner_replaces_summary() {
    let mut s = demo_state();
    s.finished = true;
    s.finish_reason = "28/28 goals met after 42 session(s)".into();
    let text = render(&s, 100, 40);
    assert!(text.contains("FINISHED"));
    assert!(text.contains("28/28 goals met"));
}

#[test]
fn color_grading() {
    assert_eq!(grade_color(1.0), Color::Green);
    assert_eq!(grade_color(0.6), Color::Yellow);
    assert_eq!(grade_color(0.3), Color::LightRed);
    assert_eq!(grade_color(0.1), Color::Red);
}

#[test]
fn human_compacts_large_counts() {
    assert_eq!(human(2_100_000), "2.1M");
    assert_eq!(human(5_000), "5.0k");
    assert_eq!(human(42), "42");
    // fmt_dur/human_bytes now live in util.rs and are tested there.
}

#[test]
fn wrap_indent_wraps_and_prefixes() {
    let out = wrap_indent("one two three four five", "↳ ", "  ", 12);
    assert!(out.len() >= 2); // wrapped onto multiple lines
    assert!(out[0].starts_with("↳ "));
    assert!(out[1].starts_with("  "));
}

#[test]
fn activity_follow_pins_to_bottom() {
    // with many events, follow-mode should scroll to show the newest (max_scroll).
    let mut s = demo_state();
    s.recent = (0..40)
        .map(|i| ActivityEvent { ts: "00:00:00".into(), kind: "tool".into(), text: format!("event {i}") })
        .collect();
    let backend = TestBackend::new(100, 40);
    let mut term = Terminal::new(backend).unwrap();
    let mut ui = DashboardUi::default(); // follow = true
    term.draw(|f| draw::draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
    let buf = term.backend().buffer().clone();
    let text: String = buf.content().iter().map(|c| c.symbol()).collect();
    // the newest event must be visible; an early one scrolled off.
    assert!(text.contains("event 39"));
}

// ── interaction model: handle_key is pure, so every keypress is unit-testable ──────────────

#[test]
fn q_and_esc_quit() {
    let mut ui = DashboardUi::default();
    assert_eq!(handle_key(&mut ui, KeyCode::Char('q')), KeyAction::Quit);
    assert_eq!(handle_key(&mut ui, KeyCode::Esc), KeyAction::Quit);
    // a non-quit key continues.
    assert_eq!(handle_key(&mut ui, KeyCode::Char('x')), KeyAction::Continue);
}

#[test]
fn tab_toggles_focus_between_judges_and_activity() {
    let mut ui = DashboardUi::default(); // starts on Activity
    assert_eq!(ui.focus, Focus::Activity);
    handle_key(&mut ui, KeyCode::Tab);
    assert_eq!(ui.focus, Focus::Judges);
    handle_key(&mut ui, KeyCode::Tab);
    assert_eq!(ui.focus, Focus::Activity);
}

#[test]
fn f_toggles_activity_follow() {
    let mut ui = DashboardUi::default(); // follow = true
    handle_key(&mut ui, KeyCode::Char('f'));
    assert!(!ui.activity_follow);
    handle_key(&mut ui, KeyCode::Char('f'));
    assert!(ui.activity_follow);
}

/// `handle_key` alone can't catch this: the pause has to survive the NEXT repaint. It did
/// not — `draw_activity` re-pins anything sitting at max_scroll, so pressing `f` in the
/// default (bottom-pinned) state toggled follow off and straight back on, and the live TUI
/// never showed `[paused]`.
#[test]
fn f_pause_survives_the_next_repaint() {
    let mut s = demo_state();
    s.recent = (0..50)
        .map(|i| ActivityEvent { ts: "00:00:00".into(), kind: "think".into(), text: format!("event {i}") })
        .collect();
    let backend = TestBackend::new(120, 40);
    let mut term = Terminal::new(backend).unwrap();
    let mut ui = DashboardUi::default(); // Activity focused, follow = true

    term.draw(|f| draw::draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
    assert!(ui.activity_follow, "starts pinned to the newest event");

    handle_key(&mut ui, KeyCode::Char('f'));
    term.draw(|f| draw::draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
    assert!(!ui.activity_follow, "an explicit `f` pause must survive the repaint");

    let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
    assert!(text.contains("paused"), "the Activity title must read [paused]");

    // and `f` again re-pins to the newest event
    handle_key(&mut ui, KeyCode::Char('f'));
    term.draw(|f| draw::draw(f, Path::new("."), Some(&s), &mut ui)).unwrap();
    assert!(ui.activity_follow);
    let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
    assert!(text.contains("live"), "the Activity title must read [⏵live] again");
}

#[test]
fn scrolling_activity_up_drops_follow_then_end_restores_it() {
    let mut ui = DashboardUi::default(); // Activity focus, follow = true
    handle_key(&mut ui, KeyCode::Up); // look back → leave follow
    assert!(!ui.activity_follow, "scrolling up must leave follow-mode");
    assert_eq!(ui.activity_scroll, 0, "already at top, clamped at 0");
    // scroll down a few, then G/End re-pins to the newest.
    handle_key(&mut ui, KeyCode::PageDown);
    assert_eq!(ui.activity_scroll, 10);
    handle_key(&mut ui, KeyCode::Char('G'));
    assert!(ui.activity_follow, "End/G must re-enable follow on the Activity pane");
}

#[test]
fn judges_pane_scrolls_and_clamps_at_top() {
    let mut ui = DashboardUi::default();
    handle_key(&mut ui, KeyCode::Tab); // focus Judges
    handle_key(&mut ui, KeyCode::Down);
    handle_key(&mut ui, KeyCode::Down);
    assert_eq!(ui.judges_scroll, 2);
    handle_key(&mut ui, KeyCode::PageUp); // -10, clamped at 0
    assert_eq!(ui.judges_scroll, 0, "scroll must never go negative");
    handle_key(&mut ui, KeyCode::Char('G')); // jump to bottom (clamped at draw time)
    assert_eq!(ui.judges_scroll, u16::MAX);
    handle_key(&mut ui, KeyCode::Char('g')); // back to top
    assert_eq!(ui.judges_scroll, 0);
}

#[test]
fn scroll_targets_only_the_focused_pane() {
    let mut ui = DashboardUi::default(); // Activity focus
    handle_key(&mut ui, KeyCode::Down);
    assert_eq!(ui.activity_scroll, 1);
    assert_eq!(ui.judges_scroll, 0, "judges pane untouched while Activity is focused");
}

// ── inject input mode: `i` opens a buffer; keys edit it (not the controls); Enter emits the text ──
#[test]
fn inject_input_mode_captures_text_and_emits_on_enter() {
    let mut ui = DashboardUi::default();
    // `i` enters input mode
    assert_eq!(handle_key(&mut ui, KeyCode::Char('i')), KeyAction::Continue);
    assert!(ui.input.is_some(), "`i` opens the inject buffer");
    // typing builds the buffer — and control keys ('q', 'f') are TEXT here, not commands
    for c in "focus q".chars() {
        assert_eq!(handle_key(&mut ui, KeyCode::Char(c)), KeyAction::Continue);
    }
    assert_eq!(ui.input.as_deref(), Some("focus q"), "typing 'q' does not quit while injecting");
    // Backspace edits
    handle_key(&mut ui, KeyCode::Backspace);
    assert_eq!(ui.input.as_deref(), Some("focus "));
    // Enter emits the trimmed text and leaves input mode
    assert_eq!(handle_key(&mut ui, KeyCode::Enter), KeyAction::Inject("focus".into()));
    assert!(ui.input.is_none(), "Enter closes the buffer");
}

#[test]
fn inject_esc_cancels_and_empty_enter_is_a_noop() {
    let mut ui = DashboardUi::default();
    handle_key(&mut ui, KeyCode::Char('i'));
    handle_key(&mut ui, KeyCode::Char('x'));
    // Esc cancels without emitting
    assert_eq!(handle_key(&mut ui, KeyCode::Esc), KeyAction::Continue);
    assert!(ui.input.is_none(), "Esc cancels inject mode");
    // Esc in normal mode still QUITS (not swallowed by inject handling)
    assert_eq!(handle_key(&mut ui, KeyCode::Esc), KeyAction::Quit);
    // whitespace-only Enter closes without an Inject action
    let mut ui = DashboardUi::default();
    handle_key(&mut ui, KeyCode::Char('i'));
    handle_key(&mut ui, KeyCode::Char(' '));
    assert_eq!(handle_key(&mut ui, KeyCode::Enter), KeyAction::Continue, "empty inject is a no-op");
    assert!(ui.input.is_none());
}
