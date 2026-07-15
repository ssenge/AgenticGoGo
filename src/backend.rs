//! The agent backend — **the abstraction over which coding agent the loop drives.**
//!
//! Which agent drives a given piece of work is resolved from config with [`for_name`] and then
//! **threaded explicitly** — there is no process-wide "current agent". Everything agent-specific
//! lives behind [`AgentBackend`]: the binary name, the flag vocabulary, the event-stream wire
//! format, the model defaults, the install probe.
//!
//! # Why there is no `active()` singleton
//! There was one: a `OnceLock` that latched on FIRST READ and **silently defaulted to Claude**.
//! The config's `model` / `effort` / `summary.model` / LLM-judge `model` defaults resolved
//! THROUGH it — so `agg.yaml` could not be PARSED without already knowing the agent, and any
//! parse in the wrong order pinned the wrong agent, silently, first-wins. It shipped that bug
//! three times: `agg init --agent codex` (fixed by using `for_name` locally), `agg doctor` on a
//! `copilot` project (reported `claude`), and `agg judge <id>`, which never selected a backend at
//! all and so ran the LLM judge on Claude for every non-Claude project.
//!
//! Those defaults are now `Option`s, resolved at **use time** against the backend actually doing
//! that piece of work (see [`crate::core::config::AggConfig::model`]). Which also means the
//! worker and the **ruler** — the backend that runs LLM judges and the summarizer — are separate
//! values that can differ, instead of one latched global that cannot.
//!
//! # Why this is a trait and not a set of free functions
//! Phase 2 of REFACTOR_1 deliberately built the *seam* (one module, no trait) on the reasoning
//! that one implementation needs no abstraction. That was right until we actually looked at
//! Codex and Copilot — and the answer changed the shape. See [`Capabilities`].
//!
//! # The capability problem — read this before adding a backend
//! Agents are NOT interchangeable, and the differences are not cosmetic. Verified against
//! `claude` 2.1.207, `codex` 0.144.1 (flags from `codex exec --help`; event schema from its Rust
//! source `codex-rs/exec/src/exec_events.rs`) and `copilot` 1.0.70 (flags from `copilot --help`;
//! event schema from the Copilot SDK streaming-events docs):
//!
//! | | Claude | Codex | Copilot |
//! |---|---|---|---|
//! | headless prompt | `-p <text>` | **positional** (`-p` is `--profile`!) | `-p <text>` |
//! | auto-approve | `--dangerously-skip-permissions` | `--dangerously-bypass-approvals-and-sandbox` | `--allow-all-tools` |
//! | event stream | `--output-format stream-json` | `--json` | `--output-format json` |
//! | output tokens | `usage.output_tokens` on the TERMINAL event | `turn.completed.usage.output_tokens` | `assistant.message.data.outputTokens` — **per message, NOT on the terminal event** |
//! | **dollar cost** | ✅ `total_cost_usd` | ❌ **NONE** (source-confirmed) | ❌ **NONE** — terminal `usage` holds `premiumRequests` + durations only |
//! | read-only one-shot | ✅ `--strict-mcp-config` + `--setting-sources user` | ✅ `--sandbox read-only` | ✅ withhold `--allow-all-tools` (tools are offered, but every write is denied) |
//! | resume | `--resume <id>` | **subcommand** `codex exec resume <ID>` | `-r/--resume=<id>`; id = `result.sessionId` |
//! | effort | `--effort` | ❌ no flag (only `-c model_reasoning_effort=…`) | `--effort` (same vocabulary) |
//! | terminal event | `type:"result"` | `turn.completed` \| `turn.failed` \| bare `error` | `type:"result"` (`sessionId`, `exitCode`, `usage`) |
//!
//! Copilot's rows are **empirical** — from actually running `copilot -p … --output-format json`
//! and reading the stream. They CONTRADICT its SDK docs, which describe a `session.shutdown`
//! terminal, an `assistant.usage` event, and no top-level session id. The real CLI emits none of
//! those: it emits `result` (carrying `sessionId`), and tokens ride on `assistant.message`. Trust
//! the wire, not the doc — and re-verify before shipping a Copilot backend, because that JSON
//! output is weeks old and GitHub has not committed to it as a stable API.
//!
//! ## The dangerous one: cost
//! `over_cost` is `cost_spent >= cost_limit`. If a backend never reports cost, `cost_spent` stays
//! 0.0, the predicate is never true, and the guard is **silently dead** — an autonomous loop with
//! `abort_if: over_cost` runs unbounded, spending real money, with no error anywhere. Same shape
//! for `over_budget`. **Neither Codex nor Copilot can price a session in dollars.**
//!
//! So a backend must DECLARE what it can report ([`Capabilities`]), the parse layer returns
//! `Option` rather than a silently-zero default ([`SessionReport`]), and [`crate::capability`]
//! refuses to start a run whose config demands something the chosen backend cannot deliver.
//! **A missing capability is a loud startup error, never a quiet no-op.** Where the agent has its
//! OWN ceiling mechanism instead (Copilot's `--max-ai-credits`), say so via
//! [`AgentBackend::spend_ceiling_hint`] so a refused guard doesn't leave the user unprotected.
//!
//! ## Three traps that will bite an implementer
//! 1. **`-p` is not universal.** In `codex exec`, `-p` means `--profile`. Porting Claude's builder
//!    by reflex would pass the entire prompt as a config-profile NAME. The prompt is positional.
//! 2. **Resume is not always a flag.** Codex resumes via a *subcommand* with a different argv
//!    shape (`codex exec resume <ID> <PROMPT>`), not `--resume`. [`AgentBackend::session_command`]
//!    returns the whole `Command` precisely so a backend can restructure argv, not just add flags.
//! 3. **Terminal events are not uniform.** Codex can end with `turn.completed`, `turn.failed`, OR
//!    a bare top-level `error` with no turn wrapper (all three confirmed on the wire). Copilot ends
//!    with `result`, whose fields are TOP-LEVEL while every other event nests under `data`. Never
//!    treat one shape as "the" terminal — and keep PROCESS EXIT as ground truth, which is what the
//!    loop already does.
//!
//! # seam
//! `agg run --sandbox` (ROADMAP P2) wraps what [`AgentBackend::session_command`] returns.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod stream;
pub mod worker;

use anyhow::Result;
use std::path::Path;
use std::process::Command;

// ---------------- what a backend can and cannot do ----------------

/// What a backend is able to REPORT or SUPPORT. Every field here exists because at least one real
/// agent lacks it — this is not speculative generality.
///
/// A `false` is not a defect to be worked around silently. It is a constraint the loop must
/// enforce at startup (see [`crate::capability::check`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Reports output-token usage on its terminal event. Required by `budget.total` / `over_budget`.
    pub reports_output_tokens: bool,
    /// Reports a DOLLAR cost. Required by `cost.total` / `over_cost`.
    ///
    /// **Only Claude does.** Codex's event schema has no cost field at all (confirmed in its
    /// source); Copilot's `assistant.usage.data.cost` is documented as a *"model multiplier cost
    /// for billing"* — a relative credit multiplier, not currency.
    ///
    /// A backend that cannot price itself in dollars must say `false` rather than let the spend
    /// guard rot into a no-op — but should then offer [`AgentBackend::spend_ceiling_hint`] if the
    /// agent can cap itself some other way. (A future agg could carry its own price table keyed by
    /// model; that would be a new capability, not a lie in this one.)
    pub reports_cost_usd: bool,
    /// Can continue a prior session's context. Required by `resume_sessions: true`.
    pub supports_resume: bool,
    /// Accepts a thinking-effort level. Required by a non-empty `effort:`.
    pub supports_effort: bool,
    /// Can distinguish a rate-limit/usage-limit from an ordinary failure, so the loop backs off
    /// instead of burning its session budget on retries. Best-effort everywhere.
    pub detects_rate_limits: bool,
    /// Can make a NON-AGENTIC single prompt→text call with tools/MCP disabled and project config
    /// ignored. Required by LLM judges and the summarizer.
    ///
    /// An LLM judge that can WRITE is not a judge — it can go and edit the thing it is grading. All
    /// three agents can be held to a read-only one-shot, but by three different mechanisms: Claude
    /// `--strict-mcp-config` + `--setting-sources user`; Codex `--sandbox read-only`; Copilot by
    /// WITHHOLDING `--allow-all-tools`, so tools are still offered but every write is denied. (An
    /// earlier survey concluded Codex and Copilot had 'no way found' and that LLM judges were
    /// Claude-only. That was wrong — verified live on all three.) Script judges are unaffected.
    pub supports_one_shot: bool,
}

// ---------------- what the loop needs back from a session ----------------

/// What the loop learns from an agent's TERMINAL event.
///
/// Every field is an `Option` on purpose: `None` means **the agent did not report this**, which is
/// categorically different from "it reported zero". Collapsing those two is exactly the bug that
/// makes a spend guard silently stop guarding.
///
/// Note what is NOT here: output tokens. They are accumulated per-line via
/// [`AgentBackend::parse_usage`], because **not every agent reports them on the terminal event** —
/// see that method for the empirical reason.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionReport {
    /// dollars this session spent, if the agent prices itself. Cumulative — SET, don't add.
    pub cost_usd: Option<f64>,
    /// the terminal event reported a rate/usage limit.
    pub rate_limited: bool,
}

/// A completed one-shot call: the model's TEXT (envelope already stripped), plus the exit status
/// and stderr the caller needs to tell "the model said no" from "the call itself failed".
///
/// It also carries USAGE (§5.6): an `.md` judge and the summarizer are LLM calls on the ruler that
/// run every step, so their spend must count against `budget`/`cost`. Before this, `one_shot` had
/// nowhere to report it and judge spend was simply uncounted.
pub struct OneShot {
    pub body: String,
    pub stderr: Vec<u8>,
    pub success: bool,
    /// output tokens this call reported (summed per line; 0 if the agent reports none).
    pub output_tokens: u64,
    /// dollars this call reported. `None` = the agent cannot price itself — a hole to surface, not a
    /// silent 0 (same discipline as [`SessionReport::cost_usd`]).
    pub cost_usd: Option<f64>,
}

/// Token + dollar spend of a single ruler call (LLM judge / summarizer), so the ceilings can sum
/// worker + judge + summarizer across agents (§5.6). `cost_usd` is `None` when the agent cannot
/// price itself — a hole to surface, never a silent 0.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spend {
    pub tokens: u64,
    pub cost_usd: Option<f64>,
}

impl Spend {
    /// From a completed one-shot's reported usage.
    pub fn from_one_shot(o: &OneShot) -> Self {
        Spend { tokens: o.output_tokens, cost_usd: o.cost_usd }
    }
}

/// Everything a backend needs to build one worker invocation. Agent-agnostic by construction — a
/// model name, an effort level, a resume handle, pass-through args. The mapping onto any
/// particular agent's FLAGS happens inside that backend's [`AgentBackend::session_command`].
pub struct SessionSpec<'a> {
    pub prompt: &'a str,
    pub model: &'a str,
    /// thinking effort; empty = don't pass it at all.
    pub effort: &'a str,
    /// continue a prior session's context instead of starting fresh.
    pub resume_id: Option<&'a str>,
    /// operator-supplied extra flags (agg.yaml `worker_args`), passed through verbatim.
    pub extra_args: &'a [String],
    /// the project directory the worker runs in.
    pub cwd: &'a Path,
}

// ---------------- the backend contract ----------------

/// One coding agent the loop can drive.
///
/// # Implementing a new backend
/// The three hard parts, in order of how badly they bite:
///
/// 1. **[`Self::capabilities`] must be honest.** Claiming `reports_cost_usd: true` when the agent
///    doesn't emit dollars re-introduces the silent-guard bug this trait exists to prevent.
/// 2. **[`Self::parse_result`] decides when a session ENDED.** Terminal semantics are not uniform
///    (Codex can end with a bare top-level `error` and no turn wrapper). Return `Some` only for a
///    genuinely terminal event. The loop treats PROCESS EXIT as ground truth regardless, so a
///    missed terminal event degrades the report, it does not hang the loop.
/// 3. **The prompt goes on ARGV, and stdin must be `/dev/null`.** Not negotiable: `codex exec`
///    hangs waiting on stdin EOF when spawned as a non-TTY child, and Copilot ignores piped stdin
///    when `-p` is given. [`worker`] already nulls stdin; keep it that way.
pub trait AgentBackend: Send + Sync {
    /// the `agent:` value in agg.yaml that selects this backend.
    fn name(&self) -> &'static str;
    /// the CLI binary, resolved via PATH (so a fake can be shimmed in tests).
    fn bin(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    /// default worker model (agg.yaml `model:`).
    fn default_model(&self) -> &'static str;
    /// default cheap model for the summarizer / LLM judge.
    fn default_summary_model(&self) -> &'static str;

    /// Build the `Command` for one interactive worker session. The caller spawns and supervises
    /// it — process groups, the stream reader, the heartbeat and the watchdog are agg's
    /// process-management concerns, not the backend's.
    fn session_command(&self, spec: &SessionSpec) -> Command;

    /// Parse one line of the agent's event stream into a displayable event, or `None` for a line
    /// that carries nothing to show.
    fn parse_event(&self, line: &str) -> Option<stream::Event>;

    /// Output tokens observed on THIS line, to be **added** to the session's running total.
    ///
    /// # Why this is per-line and not part of [`Self::parse_result`]
    /// Because agents do not agree on where usage lives, and assuming they do produces a
    /// false negative that silently disables the token budget. Observed, not guessed:
    ///
    /// - **Claude** reports cumulative `usage.output_tokens` ONCE, on its terminal `result` event.
    /// - **Copilot** reports `outputTokens` on EVERY `assistant.message` — and its terminal
    ///   `result` event carries **no token count at all** (its `usage` object holds
    ///   `premiumRequests` and durations instead). Verified by running it: an earlier design that
    ///   read tokens only from the terminal event would have forced `reports_output_tokens: false`
    ///   for Copilot and refused `over_budget` — even though Copilot reports tokens perfectly well.
    ///
    /// Summing works for BOTH shapes: an agent that reports once contributes once; an agent that
    /// reports per-message contributes many times. So this returns an INCREMENT, never a total.
    /// Return `None` for a line that carries no usage (and for an agent that never reports usage,
    /// which must also declare `reports_output_tokens: false`).
    fn parse_usage(&self, line: &str) -> Option<u64>;

    /// The resume handle, if THIS line carries it. The worker keeps the last one seen.
    ///
    /// # Also per-line, and for the same reason as [`Self::parse_usage`]
    /// Agents do not agree on WHERE the session id appears, and a terminal-only reader gets it
    /// wrong for half of them. Observed:
    ///
    /// - **Claude** and **Copilot** put it on the TERMINAL event (`session_id` / `sessionId`).
    /// - **Codex** puts it on `thread.started` — the FIRST event of the stream, not the last.
    ///
    /// So this fires on every line. An agent with no resume handle returns `None` always, and must
    /// also declare `supports_resume: false`.
    fn parse_session_id(&self, line: &str) -> Option<String>;

    /// The `effort:` agg.yaml defaults to for THIS agent. Empty = don't pass an effort at all.
    ///
    /// Backend-specific because the vocabulary is: Claude and Copilot both take
    /// `low|medium|high|xhigh|max`, and Codex takes no effort flag whatsoever. Without this, agg's
    /// blanket default of `max` would be a demand no Codex config could satisfy, and
    /// [`crate::capability::check`] would refuse EVERY Codex run out of the box — for an effort the
    /// user never actually asked for.
    fn default_effort(&self) -> &'static str {
        ""
    }

    /// Parse one line as the session's TERMINAL event. `None` = this line is not terminal.
    ///
    /// Returning `Some` with `None` fields is correct and expected for an agent that simply does
    /// not report that number — see [`SessionReport`].
    fn parse_result(&self, line: &str) -> Option<SessionReport>;

    /// A single NON-AGENTIC prompt→text call, tools/MCP off, project config ignored. Used by the
    /// LLM judge and the summarizer.
    ///
    /// Only called when [`Capabilities::supports_one_shot`] is true — [`crate::capability::check`]
    /// refuses the run otherwise, so a backend that cannot do this may simply `unreachable!()`.
    fn one_shot(&self, prompt: &str, model: &str, timeout_secs: u64, cwd: Option<&Path>) -> Result<OneShot, String>;

    /// Tally a one-shot's usage from its RAW stdout by re-using this backend's own per-line parsers
    /// ([`Self::parse_usage`] / [`Self::parse_result`]) — so `OneShot` carries token/cost spend
    /// (§5.6) without every backend hand-rolling it. Claude emits ONE JSON object (parse the blob);
    /// Codex/Copilot stream one per line (sum them). Returns `(output_tokens, cost_usd)`.
    fn tally_one_shot(&self, raw_stdout: &[u8]) -> (u64, Option<f64>) {
        let text = String::from_utf8_lossy(raw_stdout);
        let whole = text.trim();
        // single-object shape (Claude's `--output-format json`): read the blob, don't also sum lines.
        if self.parse_usage(whole).is_some() || self.parse_result(whole).is_some() {
            let tokens = self.parse_usage(whole).unwrap_or(0);
            let cost = self.parse_result(whole).and_then(|r| r.cost_usd);
            return (tokens, cost);
        }
        // streamed shape (Codex/Copilot): sum per line, last cost wins.
        let mut tokens = 0u64;
        let mut cost = None;
        for line in text.lines() {
            if let Some(t) = self.parse_usage(line) {
                tokens += t;
            }
            if let Some(r) = self.parse_result(line) {
                if let Some(c) = r.cost_usd {
                    cost = Some(c);
                }
            }
        }
        (tokens, cost)
    }

    /// Two config keys that are each individually LEGAL, but that this agent cannot accept
    /// TOGETHER. Return the explanation; `None` (the default) means the agent has no such pair.
    ///
    /// # Why this is separate from [`Capabilities`]
    /// `Capabilities` answers "can the agent do X at all?" — a property of the agent. This answers
    /// "can it do X *and* Y at once?" — a property of the COMBINATION, which no per-feature flag
    /// can express. Copilot is the reason it exists: it supports `--effort`, and it supports
    /// `--model auto`, so both capability flags are honestly `true` — but ask for both and it
    /// refuses the invocation outright:
    ///
    /// ```text
    /// Error: Model "auto" does not support reasoning effort configuration (requested: "max").
    /// ```
    ///
    /// Every worker session then dies in ~4s with exit 1 and zero tokens, while the loop happily
    /// runs sessions, judges them, and halts on `over_iterations` having achieved nothing. Before
    /// this hook, `agg doctor` printed "✔ agent `copilot` can do everything this config asks" for
    /// exactly that config — a green light on a run that cannot work.
    ///
    /// Checked at startup by [`crate::capability::check`], so it is a loud refusal, never a silent
    /// dropped flag.
    fn config_conflict(&self, _model: &str, _effort: &str) -> Option<String> {
        None
    }

    /// How this agent can cap its OWN spend, when it cannot report cost to us in dollars.
    ///
    /// Refusing `cost.total` on a backend with `reports_cost_usd: false` is correct — but on its
    /// own it leaves the operator with NO spend protection at all, which is the very outcome the
    /// refusal exists to prevent. If the agent has a native ceiling, name it here and
    /// [`crate::capability::check`] will put it in the error.
    ///
    /// Copilot, for example, takes `--max-ai-credits <n>` (it bills in GitHub AI Credits, not
    /// dollars) — passable through agg.yaml's `worker_args`. Claude returns `None`: agg's own cost
    /// guard works, so there is nothing to fall back to.
    fn spend_ceiling_hint(&self) -> Option<&'static str> {
        None
    }

    /// Is the agent CLI on PATH and runnable?
    fn is_installed(&self) -> bool;

    /// Hard preflight: bail with an install hint if the CLI is missing.
    fn preflight(&self) -> Result<()>;
}

// ---------------- selection ----------------

/// Does this text look like a rate/usage-limit error?
///
/// Shared by every backend, because none of them expose a machine-readable error CODE for it —
/// Claude's terminal event carries prose, and Codex's `ThreadErrorEvent` is literally
/// `{message: String}`. Substring matching is all anyone has, so at least they all use the SAME
/// list, and each backend gates it to its TERMINAL failure events only. Never scan tool output: a
/// command that merely prints "429" must not park the loop in a 30-minute backoff.
pub fn looks_rate_limited(text: &str) -> bool {
    const PATS: &[&str] = &[
        // ---- Anthropic / Claude prose ----
        "rate_limit_error",
        "usage limit reached",
        "overloaded_error",
        // ---- OpenAI / Codex ----
        // Codex does NOT hand us prose: its terminal event's `message` is the RAW UPSTREAM JSON,
        // verified on the wire by forcing a 400:
        //   {"type":"error","message":"{\"type\":\"error\",\"status\":400,
        //     \"error\":{\"type\":\"invalid_request_error\",\"message\":\"…\"}}"}
        // So a real 429 arrives as `"status":429` and `"type":"rate_limit_exceeded"` — NONE of
        // which the Claude-shaped patterns above match. Before these three, Codex declared
        // `detects_rate_limits: true` and detected nothing: a rate-limited loop would skip the
        // backoff, treat the 429 as an ordinary failure, and immediately burn the next session
        // against the wall. Match the JSON form, not the prose.
        "\"status\":429",
        "\"status\": 429",
        "rate_limit_exceeded",
        "rate limit reached",
        // ---- shape-agnostic ----
        "status 429",
        "http 429",
        "429 too many requests",
        "too many requests",
        "quota_exceeded", // Copilot returns HTTP 402 with this
        "insufficient_quota",
    ];
    let h = text.to_lowercase();
    PATS.iter().any(|p| h.contains(p))
}

/// Every backend agg knows how to drive. Adding one = a new `claude.rs`-shaped module + one arm.
pub const KNOWN: &[&str] = &["claude", "codex", "copilot"];

/// Resolve an `agent:` name to its backend.
pub fn for_name(name: &str) -> Result<&'static dyn AgentBackend> {
    match name {
        "claude" => Ok(&claude::Claude),
        "codex" => Ok(&codex::Codex),
        "copilot" => Ok(&copilot::Copilot),
        other => anyhow::bail!(
            "unknown agent `{other}` in agg.yaml.\n  known agents: {}\n  \
             (adding one means implementing `trait AgentBackend` — see src/backend.rs)",
            KNOWN.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
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
}
