use super::*;
use std::time::Instant;

/// The wire form is a cross-version contract — it must be byte-for-byte what it always was.
#[test]
fn phase_round_trips_through_its_legacy_wire_strings() {
    for (p, wire) in [
        (Phase::Starting, "starting"),
        (Phase::Inject, "inject"),
        (Phase::Run, "run"),
        (Phase::Verify, "verify"),
        (Phase::Gate, "gate"),
        (Phase::Backoff, "backoff"),
        (Phase::Done, "done"),
    ] {
        assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{wire}\""));
        assert_eq!(serde_json::from_str::<Phase>(&format!("\"{wire}\"")).unwrap(), p);
        assert_eq!(p.to_string(), wire);
    }
}

/// A stage written by a DIFFERENT agg build (an old one wrote "judging") must neither crash
/// the reader nor lose its name — `agg dashboard` attaches to loops it didn't launch.
#[test]
fn an_unknown_phase_survives_verbatim() {
    let p: Phase = serde_json::from_str("\"judging\"").expect("must not reject a foreign stage");
    assert_eq!(p, Phase::Other("judging".into()));
    assert_eq!(p.to_string(), "judging", "it must still render its real name");
    assert_eq!(serde_json::to_string(&p).unwrap(), "\"judging\"", "and round-trip unchanged");
}

/// A state.json written by an OLDER `agg` (no model/halt_when/started_at/recent,
/// goals without `weight`) must still deserialize — the new fields fall back to
/// their defaults via `#[serde(default)]`. Guards the in-place-upgrade-mid-run case.
#[test]
fn old_schema_deserializes_with_defaults() {
    let old = r#"{
        "project":"telos","stop_when":"mip28_optimal","up_secs":4705,"session":2,
        "phase":"judging","idle_secs":0,"tokens_spent":588117,"budget_total":null,
        "goals_met":2,"goals_total":3,
        "goals":[{"id":"g","goal_type":"cardinal","state":"in_progress","invariant":false,
                  "value":18.0,"max":28.0,"target":28.0,"delta":0.0,
                  "rationale":"18/28","judge_kind":"script"}],
        "now":"x","think":"y","summary_cumulative":"s","summary_windowed":"w",
        "seq":12,"finished":false,"finish_reason":""
    }"#;
    let s: DashboardState = serde_json::from_str(old).expect("old schema must parse");
    assert_eq!(s.project, "telos");
    assert_eq!(s.session, 2);
    // new fields defaulted, not errored:
    assert_eq!(s.model, "");
    assert_eq!(s.halt_when, "");
    assert_eq!(s.started_at_epoch, 0);
    assert!(s.recent.is_empty());
    // a goal missing `weight` defaults to 0.0 (rendered as "w0").
    assert_eq!(s.goals[0].weight, 0.0);
    assert_eq!(s.goals[0].value, 18.0);
}

#[test]
fn push_event_caps_the_ring_and_tracks_now_think() {
    let mut s = DashboardState::default();
    for i in 0..(RECENT_CAP + 10) {
        s.push_event(ActivityEvent {
            ts: "00:00:00".into(),
            kind: "tool".into(),
            text: format!("cmd {i}"),
        });
    }
    assert_eq!(s.recent.len(), RECENT_CAP); // capped
    assert_eq!(s.recent.last().unwrap().text, format!("cmd {}", RECENT_CAP + 9)); // newest kept
    assert_eq!(s.recent.first().unwrap().text, "cmd 10"); // oldest dropped
    assert_eq!(s.now, format!("cmd {}", RECENT_CAP + 9)); // tool updates `now`

    s.push_event(ActivityEvent { ts: "00:00:01".into(), kind: "think".into(), text: "pondering".into() });
    assert_eq!(s.think, "pondering"); // think updates `think` (and `now`)
    assert_eq!(s.now, "pondering");
}

#[test]
fn live_state_publishes_to_disk() {
    let dir = std::env::temp_dir().join(format!("agg_live_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let live = LiveState::new(&dir, Instant::now(), DashboardState { project: "p".into(), ..Default::default() });
    live.update(|s| s.push_event(ActivityEvent { ts: "t".into(), kind: "tool".into(), text: "go".into() }));
    let read = DashboardState::read(&dir).expect("state.json written");
    assert_eq!(read.project, "p");
    assert_eq!(read.recent.len(), 1);
    assert_eq!(read.now, "go");
    assert!(read.seq >= 1);
    let _ = std::fs::remove_dir_all(&dir);
}
