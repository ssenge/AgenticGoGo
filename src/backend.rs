//! The agent backend — **the abstraction over which coding agent the loop drives.**
//!
//! One process drives exactly one agent, chosen at startup by `agg.yaml`'s `agent:` key and held
//! in [`active`]. Everything agent-specific lives behind [`AgentBackend`]: the binary name, the
//! flag vocabulary, the event-stream wire format, the model defaults, the install probe.
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
//! | output tokens | `usage.output_tokens` | `turn.completed.usage.output_tokens` | `assistant.usage.data.outputTokens` |
//! | **dollar cost** | ✅ `total_cost_usd` | ❌ **NONE** (source-confirmed) | ❌ `cost` = a *billing multiplier*, not USD |
//! | tools-off one-shot | ✅ | ❌ no way found | ⚠️ maybe (`--available-tools=` + `--disable-builtin-mcps`), unverified |
//! | resume | `--resume <id>` | **subcommand** `codex exec resume <ID>` | `-r/--resume=<id>` |
//! | effort | `--effort` | ❌ no flag (only `-c model_reasoning_effort=…`) | `--effort` (same vocabulary) |
//! | terminal event | `type:"result"` | `turn.completed` \| `turn.failed` \| bare `error` | `session.shutdown` (`shutdownType`) |
//!
//! ## The dangerous one: cost
//! `over_cost` is `cost_spent >= cost_limit`. If a backend never reports cost, `cost_spent` stays
//! 0.0, the predicate is never true, and the guard is **silently dead** — an autonomous loop with
//! `halt_when: over_cost` runs unbounded, spending real money, with no error anywhere. Same shape
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
//!    a bare top-level `error` with no turn wrapper. Copilot ends with `session.shutdown` carrying
//!    `shutdownType: routine|error`. Never treat one shape as "the" terminal — and keep PROCESS
//!    EXIT as ground truth, which is what the loop already does.
//!
//! # seam
//! `agg run --sandbox` (ROADMAP P2) wraps what [`AgentBackend::session_command`] returns.

pub mod claude;
pub mod stream;
pub mod worker;

use anyhow::Result;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

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
    /// This is the one most likely to be `false`, and it is not obvious: Codex and Copilot are
    /// architecturally agentic even for one-shot calls, and neither has a verified way to fully
    /// disable tools. An LLM judge that can run tools is not a judge — it can go and edit the
    /// thing it is grading. Script judges are unaffected.
    pub supports_one_shot: bool,
}

// ---------------- what the loop needs back from a session ----------------

/// Everything the loop learns from an agent's TERMINAL event.
///
/// Every field is an `Option` on purpose: `None` means **the agent did not report this**, which is
/// categorically different from "it reported zero". Collapsing those two is exactly the bug that
/// makes a spend guard silently stop guarding.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionReport {
    /// handle for a later `--resume`, if the agent exposes one.
    pub session_id: Option<String>,
    /// output-side tokens this session spent.
    pub output_tokens: Option<u64>,
    /// dollars this session spent, if the agent prices itself.
    pub cost_usd: Option<f64>,
    /// the terminal event reported a rate/usage limit.
    pub rate_limited: bool,
}

/// A completed one-shot call: the model's TEXT (envelope already stripped), plus the exit status
/// and stderr the caller needs to tell "the model said no" from "the call itself failed".
pub struct OneShot {
    pub body: String,
    pub stderr: Vec<u8>,
    pub success: bool,
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

/// Every backend agg knows how to drive. Adding one = a new `claude.rs`-shaped module + one arm.
pub const KNOWN: &[&str] = &["claude"];

/// Resolve an `agent:` name to its backend.
pub fn for_name(name: &str) -> Result<&'static dyn AgentBackend> {
    match name {
        "claude" => Ok(&claude::Claude),
        other => anyhow::bail!(
            "unknown agent `{other}` in agg.yaml.\n  known agents: {}\n  \
             (adding one means implementing `trait AgentBackend` — see src/backend.rs)",
            KNOWN.join(", ")
        ),
    }
}

/// The backend this process drives. One process, one agent — selected once at startup.
static ACTIVE: OnceLock<&'static dyn AgentBackend> = OnceLock::new();

/// Select the backend for this process. Call once, at startup, from the `agent:` config key.
/// Idempotent; a second call with a different agent is ignored (the first wins).
pub fn init(name: &str) -> Result<()> {
    let b = for_name(name)?;
    let _ = ACTIVE.set(b);
    Ok(())
}

/// The active backend. Defaults to Claude when [`init`] was never called — which is what every
/// path that predates multi-agent support (and every test that doesn't care) gets.
pub fn active() -> &'static dyn AgentBackend {
    *ACTIVE.get_or_init(|| &claude::Claude)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_agent_is_a_loud_error_that_lists_the_known_ones() {
        // `.err()` not `.unwrap_err()`: the Ok type is `&dyn AgentBackend`, which has no Debug.
        let e = for_name("codex").err().expect("an unknown agent must be an error").to_string();
        assert!(e.contains("unknown agent `codex`"), "got: {e}");
        assert!(e.contains("claude"), "the error must list what IS supported, got: {e}");
    }

    #[test]
    fn the_default_backend_is_claude() {
        assert_eq!(active().name(), "claude");
        assert_eq!(active().bin(), "claude");
    }

    /// The whole point of `Capabilities`: a backend that cannot price itself must SAY so. If this
    /// ever flips to a blanket `true` for a non-reporting agent, the spend guard dies silently.
    #[test]
    fn claude_declares_the_capabilities_it_actually_has() {
        let c = active().capabilities();
        assert!(c.reports_output_tokens && c.reports_cost_usd, "claude reports both");
        assert!(c.supports_one_shot, "claude can run a tools-off judge call");
        assert!(c.supports_resume && c.supports_effort && c.detects_rate_limits);
    }
}
