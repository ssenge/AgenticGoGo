//! Unit tests for the backend module: agent selection ([`super::for_name`] /
//! [`super::KNOWN`]), the shared rate-limit detector, and the capability /
//! usage-reporting contract, exercised through the Claude backend as a concrete
//! stand-in resolved by name (there is no ambient default any more).

use super::*;

#[test]
fn unknown_agent_is_a_loud_error_that_lists_the_known_ones() {
    // `.err()` not `.unwrap_err()`: the Ok type is `&dyn AgentBackend`, which has no Debug.
    let e = for_name("gemini").err().expect("an unknown agent must be an error").to_string();
    assert!(e.contains("unknown agent `gemini`"), "got: {e}");
    // …and it must list what IS supported, so the user can fix it without reading the source.
    for known in KNOWN {
        assert!(e.contains(known), "the error must list `{known}`, got: {e}");
    }
}

/// The claude backend used by the tests below as a concrete stand-in. It is resolved BY NAME,
/// like production does — there is no ambient default to fall back on any more.
fn claude() -> &'static dyn AgentBackend {
    for_name("claude").expect("claude is a known agent")
}

#[test]
fn the_default_agent_key_is_claude() {
    assert_eq!(claude().name(), "claude");
    assert_eq!(claude().bin(), "claude");
}

#[test]
fn every_known_agent_actually_resolves() {
    for name in KNOWN {
        let b = for_name(name).unwrap_or_else(|_| panic!("KNOWN lists `{name}` but for_name rejects it"));
        assert_eq!(&b.name(), name, "a backend's name() must match the `agent:` value that selects it");
    }
}

/// REGRESSION — the rate-limit detector must match the shape each agent ACTUALLY sends.
///
/// The pattern list was written against Claude, which reports prose (`rate_limit_error`).
/// Codex does not: its terminal event's `message` is the RAW UPSTREAM JSON. Captured from the
/// wire by forcing a 400 (`codex exec --model definitely-not-a-real-model --json`):
///
/// ```text
/// {"type":"turn.failed","error":{"message":"{\"type\":\"error\",\"status\":400,
///   \"error\":{\"type\":\"invalid_request_error\",\"message\":\"…\"}}"}}
/// ```
///
/// So a real 429 carries `"status":429` and `"rate_limit_exceeded"` — and matched NOTHING in
/// the Claude-shaped list. Codex therefore declared `detects_rate_limits: true` while detecting
/// nothing at all: on a real rate limit the loop would skip its backoff, score the 429 as an
/// ordinary failure, and immediately spawn the next session into the same wall, burning the
/// session budget. Both agents' real shapes are asserted here so neither can regress.
#[test]
fn the_rate_limit_detector_matches_what_each_agent_really_sends() {
    // Codex: the exact envelope observed on the wire, with 429 in place of the captured 400.
    let codex_429 = r#"{"type":"error","status":429,"error":{"type":"rate_limit_exceeded","message":"Rate limit reached for gpt-5-codex in organization org-x on requests per min (RPM): Limit 500, Used 500."}}"#;
    assert!(
        looks_rate_limited(codex_429),
        "a REAL codex 429 must trip the backoff — it carries JSON, not prose"
    );
    // …and it must survive the turn.failed wrapper the loop actually feeds it.
    let wrapped = format!(r#"{{"type":"turn.failed","error":{{"message":{codex_429:?}}}}}"#);
    assert!(looks_rate_limited(&wrapped), "…including nested in turn.failed");

    // Claude's prose shape must keep working — this is a widening, not a replacement.
    assert!(looks_rate_limited(r#"{"type":"result","result":"rate_limit_error: slow down"}"#));
    assert!(looks_rate_limited("Usage limit reached — resets at 5pm"));
    // Codex SUBSCRIPTION exhaustion — prose, no 429, no "reached". Captured verbatim on the wire
    // during a real `agg run` on 2026-08-05, where it matched nothing: the loop scored a 4-second
    // 0-token failure as ordinary work and burned one of that step's two `max:` attempts on it.
    assert!(looks_rate_limited(
        "You've hit your usage limit. To continue using Codex and get access to GPT-5.3-Codex, \
         start a free trial of Plus today (https://chatgpt.com/explore/plus), or try again at \
         Aug 11th, 2026 2:06 PM."
    ));
    // Copilot's 402.
    assert!(looks_rate_limited(r#"{"error":{"code":"quota_exceeded"}}"#));

    // And the thing that must NOT trip a 30-minute backoff: an ordinary failure, and a build
    // log that merely mentions the number.
    assert!(!looks_rate_limited(
        r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"model not supported"}}"#
    ));
    assert!(!looks_rate_limited("test_429_parsing ... ok"));
}

/// REGRESSION — a backend's OWN defaults must not contradict each other.
///
/// Copilot shipped `default_model() == "auto"` together with `default_effort() == "max"`, and
/// Copilot rejects that pair outright: *"Model `auto` does not support reasoning effort
/// configuration"*. The out-of-the-box config — which names neither, so both defaults apply —
/// therefore killed EVERY worker session in 4s with exit 1 and zero tokens, while the loop
/// happily ran its sessions and halted on `over_iterations` having achieved nothing.
///
/// Only a live `agg run` caught it, because the contradiction lives in the agent's own argv
/// validation, not in ours. This test cannot re-run the CLI, so it enforces the invariant that
/// makes the trap impossible: `auto` means "you pick", and an agent picking its own model
/// cannot also be told how hard to think. If a backend defaults to `auto`, it must not also
/// default to an effort.
#[test]
fn no_backend_defaults_to_an_effort_its_default_model_cannot_accept() {
    for name in KNOWN {
        let b = for_name(name).unwrap();
        if b.default_model() == "auto" {
            assert!(
                b.default_effort().is_empty(),
                "`{name}` defaults to model `auto` AND effort `{}` — the agent will reject the \
                 pair and every worker session dies instantly. One of them must give.",
                b.default_effort()
            );
        }
    }
}

/// REGRESSION — the successor to `the_agent_key_is_readable_without_latching_the_backend`, and
/// the test for the whole class of bug that killed `active()`.
///
/// The old shape: agg.yaml's `model` / `summary.model` defaults were BACKEND-SPECIFIC, so
/// resolving one read the `active()` OnceLock — which LATCHED, silently, on Claude. A config
/// parse before the agent was selected therefore pinned the WRONG agent for the whole process
/// (first-wins), and the loop drove an agent the user never asked for. The old test worked
/// around it, by proving `agent:` could be read WITHOUT a full parse. This one pins the fix:
/// there is nothing to latch, so a full parse is safe in ANY order, and an absent `model:` is
/// `None` — "ask the backend at USE time" — rather than a value baked in at PARSE time.
#[test]
fn config_parses_without_a_backend_and_defaults_resolve_at_use_time() {
    use crate::core::config::AggConfig;
    let dir = std::env::temp_dir().join(format!("agg-agentkey-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // The exact shape that triggered the bug: names copilot under `defaults:`, and OMITS
    // `model:` — the missing key is what used to force a backend-specific default to resolve
    // mid-parse. (`resume_prompt` is gone; `deny_unknown_fields` would now reject it outright.)
    let p = dir.join("agg.yaml");
    let copilot_cfg = "project: x\ndefaults: { agent: copilot }\nsteps: { work: {} }\nsequence: { steps: [work] }\n";
    std::fs::write(&p, copilot_cfg).unwrap();
    let cfg = AggConfig::load(&p).expect("a full parse must not need a backend");
    assert_eq!(cfg.defaults.agent, "copilot");
    assert!(cfg.defaults.model.is_none(), "an absent `model:` must stay None, not bake in a default");
    // BOTH readers must see the SAME key. `agent_name` parses its own private partial, and it —
    // not `load()` — is what picks the backend for `judge`, `plan`, `doctor` and `skills
    // install`. Let the two drift (a `#[serde(rename)]` on one, say) and `agg judge` silently
    // runs the LLM judge on Claude for a copilot project: the exact bug this test is named for,
    // back again, with the full-parse assertion above still green.
    assert_eq!(AggConfig::agent_name(&p), "copilot", "agent_name must read the real key");

    // …and the WORKER model resolves against the agent the CONFIG names — not whichever backend
    // was touched first. Absent `model:` means "ask the step's backend at USE time". This is the
    // assertion the OnceLock could not have satisfied.
    let copilot = for_name(&cfg.defaults.agent).unwrap();
    let step = cfg.resolve_step("work").unwrap();
    assert_eq!(step.model(copilot), copilot.default_model());
    assert_ne!(step.model(copilot), claude().default_model(), "…and it is NOT claude's");
    // an EXPLICIT model wins over any backend default, on every backend.
    std::fs::write(&p, "project: x\ndefaults: { agent: copilot, model: pinned }\nsteps: { work: {} }\nsequence: { steps: [work] }\n").unwrap();
    let cfg = AggConfig::load(&p).unwrap();
    let step = cfg.resolve_step("work").unwrap();
    assert_eq!(step.model(copilot), "pinned");
    assert_eq!(step.model(claude()), "pinned");

    // `agent_name` survives for the paths where agg.yaml may not exist or may not parse
    // (doctor / plan / judge / skills install): absent or agent-less → the default.
    std::fs::write(&p, "project: x\nsequence: { steps: [work] }\n").unwrap();
    assert_eq!(AggConfig::agent_name(&p), "claude");
    assert_eq!(AggConfig::agent_name(&dir.join("nope.yaml")), "claude");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The whole point of `Capabilities`: a backend that cannot price itself must SAY so. If this
/// ever flips to a blanket `true` for a non-reporting agent, the spend guard dies silently.
#[test]
fn claude_declares_the_capabilities_it_actually_has() {
    let c = claude().capabilities();
    assert!(c.reports_output_tokens && c.reports_cost_usd, "claude reports both");
    assert!(c.supports_one_shot, "claude can run a tools-off judge call");
    assert!(c.supports_resume && c.supports_effort && c.detects_rate_limits);
}

/// `parse_usage` returns an INCREMENT per line, which is what lets ONE worker loop serve two
/// incompatible reporting shapes. Claude reports once, on its terminal event. Copilot reports
/// on every assistant message and puts NO token count on its terminal event — an earlier design
/// read tokens only from the terminal event, which would have forced Copilot to declare
/// `reports_output_tokens: false` and silently refused `over_budget` on an agent that reports
/// tokens perfectly well. Summing serves both; totalling does not.
#[test]
fn usage_is_an_increment_so_report_once_and_report_per_message_both_work() {
    let b = claude();
    // Claude's shape: usage rides the terminal `result` event, once.
    let result_line = r#"{"type":"result","usage":{"output_tokens":100,"cache_creation_input_tokens":5},"total_cost_usd":0.25,"session_id":"s1"}"#;
    assert_eq!(b.parse_usage(result_line), Some(105), "output + cache-creation are both output-priced");

    // A line with no usage contributes NOTHING — and `None` is not `Some(0)`. That distinction
    // is what keeps "the agent didn't report" from masquerading as "the agent reported zero".
    assert_eq!(b.parse_usage(r#"{"type":"assistant","message":{}}"#), None);
    assert_eq!(b.parse_usage(r#"{"type":"result","session_id":"s1"}"#), None, "result with no usage object");

    // Summing the stream is the worker's job; here we prove the pieces add up.
    let total: u64 = [result_line, r#"{"type":"assistant"}"#]
        .iter()
        .filter_map(|l| b.parse_usage(l))
        .sum();
    assert_eq!(total, 105);
}

/// The terminal event still owns the CUMULATIVE facts (id, cost, rate-limit) — those are SET,
/// not summed, and must not be confused with the incremental usage above.
#[test]
fn the_terminal_event_owns_the_cumulative_facts() {
    let b = claude();
    let line = r#"{"type":"result","total_cost_usd":0.25,"session_id":"s1"}"#;
    let r = b.parse_result(line).expect("a result line is terminal");
    assert_eq!(r.cost_usd, Some(0.25));
    assert!(!r.rate_limited);
    // Claude's resume handle is on the terminal event; Codex's is on the FIRST — which is why
    // it's a per-line method rather than a SessionReport field.
    assert_eq!(b.parse_session_id(line).as_deref(), Some("s1"));
    // a non-terminal line is not a report at all
    assert!(b.parse_result(r#"{"type":"assistant"}"#).is_none());
    // …and an agent that reports no cost yields None, NOT Some(0.0)
    let r = b.parse_result(r#"{"type":"result","session_id":"s1"}"#).unwrap();
    assert_eq!(r.cost_usd, None, "absent cost must be None — Some(0.0) would disarm over_cost");
}
