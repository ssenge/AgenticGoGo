//! The data contract between the loop and a backend: what a worker session is GIVEN
//! ([`SessionSpec`]) and what a run REPORTS back ([`Capabilities`], [`SessionReport`],
//! [`OneShot`], [`Spend`]). These are pure data types; the behavioural contract
//! ([`super::AgentBackend`]) and agent selection stay in the parent module.

use std::path::Path;

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
    /// when the limit resets, as an ABSOLUTE epoch (secs), if the message said so (Claude
    /// subscription limits carry `resets <time>`). `None` = unknown → the backoff falls back to the
    /// fixed `ratelimit_backoff_secs`. Absolute (not a duration) so it survives the gap between the
    /// reader parsing it and the backoff handler acting on it.
    pub rate_limit_reset: Option<u64>,
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
    /// blast-radius isolation for this session. A backend that self-sandboxes (Codex) reads this to
    /// pick its native flags; others ignore it and get wrapped by the OS sandbox in [`super::worker`].
    pub isolation: crate::isolation::Isolation,
}
