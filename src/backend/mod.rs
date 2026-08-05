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

mod contract;
pub use contract::*;

#[cfg(test)]
mod tests;

use anyhow::Result;
use std::path::Path;
use std::process::Command;

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
    /// `isolation` confines it exactly like a worker session (ISOLATION.md §12): a judge grading a
    /// sandboxed step must not itself be an escape hatch. The LLM judge already denies tool writes
    /// at the agent-permission layer, but that is defense-in-depth, "NOT a host jail" — so under
    /// `Sandbox` EVERY backend gets the real OS wrapper here too (§2.4; Codex's own
    /// `--sandbox read-only` no longer exempts it). Requires a `cwd` to anchor the jail; a `None`
    /// cwd (the summarizer) is never confined.
    ///
    /// Only called when [`Capabilities::supports_one_shot`] is true — [`crate::capability::check`]
    /// refuses the run otherwise, so a backend that cannot do this may simply `unreachable!()`.
    fn one_shot(
        &self,
        prompt: &str,
        model: &str,
        timeout_secs: u64,
        cwd: Option<&Path>,
        isolation: crate::isolation::Isolation,
    ) -> Result<OneShot, String>;

    /// Wrap a one-shot's built [`Command`] in the OS sandbox whenever the step is confined — the
    /// shared tail of every `one_shot`. A `None` cwd cannot be jailed (no anchor), so it is
    /// returned unwrapped.
    ///
    /// **Applied to EVERY backend, including one that confines itself.** A judge that runs under
    /// the agent's own `--sandbox read-only` is confined by the agent's promise; agg's carve-out
    /// (`agg/private/`, the verdict ledger it is about to append to) exists precisely because that
    /// promise is not agg's to make. The two layers compose — the outer one is ours.
    fn confine_one_shot(
        &self,
        command: Command,
        cwd: Option<&Path>,
        isolation: crate::isolation::Isolation,
    ) -> Result<Command, String> {
        match cwd {
            Some(dir) if isolation == crate::isolation::Isolation::Sandbox => {
                // `&[]`: a one-shot is not a step, so there is no per-step `readonly` list to
                // deliver — only the derived `agg/private/` carve-out, which `wrap` adds itself.
                crate::isolation::wrap(command, dir, &self.writable_state_paths(), &[])
                    .map_err(|e| e.to_string())
            }
            _ => Ok(command),
        }
    }

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

    /// Can this agent ADDITIONALLY confine itself, on top of agg's jail?
    ///
    /// ⛔ **This does NOT gate the OS wrapper.** Under `isolation: sandbox` agg wraps EVERY agent
    /// unconditionally ([`worker::run_session`], [`Self::confine_one_shot`]) — an agent's own
    /// sandbox is the agent's promise, and agg's moat does not rest on promises. It used to mean
    /// "skip the wrapper for this agent"; that rule is deleted.
    ///
    /// What survives is REPORTING: `true` says the agent has a second, inner kernel jail of its own
    /// (only Codex — `sandbox_mode=workspace-write`, Seatbelt/Landlock under the hood), which its
    /// [`Self::session_command`] switches on by reading `spec.isolation`. Claude and Copilot have
    /// permission layers, not kernel jails, so they return `false`. Nothing in the confinement path
    /// branches on it; `agg doctor` and diagnostics may.
    fn self_sandboxes(&self) -> bool {
        false
    }

    /// Directories OUTSIDE cwd that the agent must be able to WRITE while sandboxed — its session
    /// logs / cache (e.g. `~/.claude`, `~/.copilot`). Only dirs that actually EXIST are returned, so
    /// the wrapper never binds a nonexistent path. Reads are already covered by "read everything";
    /// this is the write carve-out only. Empty for a backend that writes nothing outside cwd (Codex,
    /// which self-sandboxes anyway).
    fn writable_state_paths(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    /// Is the agent CLI on PATH and runnable?
    fn is_installed(&self) -> bool;

    /// Hard preflight: bail with an install hint if the CLI is missing.
    fn preflight(&self) -> Result<()>;
}

/// The `~/<name>` agent-state paths that exist — the writable carve-out for the OS sandbox
/// ([`AgentBackend::writable_state_paths`]). Returns only what is actually there, so the wrapper
/// never binds a path that isn't (and empty when there is no HOME).
///
/// FILES count, not just dirs: Claude's primary config is `~/.claude.json`, a plain file SIBLING to
/// `~/.claude`, so a dirs-only rule silently loses every write to it (measured — `Operation not
/// permitted`, and the CLI does not complain).
fn state_paths_that_exist(names: &[&str]) -> Vec<std::path::PathBuf> {
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    let home = std::path::Path::new(&home);
    names.iter().map(|n| home.join(n)).filter(|p| p.exists()).collect()
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
        // ⚠ "usage limit", NOT "usage limit reached". Codex's SUBSCRIPTION exhaustion is prose with
        // no 429 and no `reached`, captured on the wire 2026-08-05 during a real sample run:
        //   "You've hit your usage limit. To continue using Codex and get access to GPT-5.3-Codex,
        //    start a free trial of Plus today (…), or try again at Aug 11th, 2026 2:06 PM."
        // It matched NOTHING in this list — the third time that has happened here, and each time the
        // consequence is the same: no backoff, the failed session is scored as ordinary work, and its
        // `until:`/`max:` attempt is burned on an agent that never ran. In that run it consumed one of
        // `spec`'s two attempts in 4 seconds for 0 tokens. The wider pattern subsumes the API's
        // "usage limit reached" too, so this is one entry rather than two.
        "usage limit",
        "overloaded_error",
        // Claude Code SUBSCRIPTION limits (Pro/Max) — a DIFFERENT wording from the API "usage
        // limit reached" above, and the one a subscription actually hits. Verified on the wire:
        //   "You've hit your session limit · resets 12:50pm (Europe/Berlin)"
        // Before this, that message matched NOTHING here, so a subscription rate-limit went
        // undetected: no backoff, the zero-token retries tripped the dud-worker abort, and the run
        // died ~1 min after hitting the wall instead of waiting for the reset. See `reset_epoch`.
        "session limit",
        "weekly limit",
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
