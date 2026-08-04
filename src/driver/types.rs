//! The public value types a driver names, matches on and stores.
//!
//! Everything here is deliberately plain data. The one exception is [`Agent::Custom`], which
//! carries a live backend — see its doc for why that is the documented break in the "one struct,
//! two constructors" claim.

use crate::backend::AgentBackend;
use crate::core::model::Verdict;
use crate::plugin::RunOutcome;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// Fatal — the one error type a driver's `main` returns
// ---------------------------------------------------------------------------------------------

/// Everything that can end a driver call unsuccessfully, in one type so `fn main() -> Result<(), Fatal>`
/// works and a driver's own `?` on a file operation composes with agg's.
///
/// # Why `Ended` is an ERROR and not an `Ok` value
///
/// A breached ceiling, `agg stop` and Ctrl-C are all *enforcement*: the driver must stop doing work,
/// and the cheapest way to make that true for code that was written without thinking about it is
/// `?`. The latch (BUILD.md §3.3) then makes every subsequent `agg.*` call a no-op, so even a driver
/// that swallows the `Err` and loops spends nothing further.
///
/// # `#[non_exhaustive]`
///
/// A driver that matches on `Fatal` must keep compiling when agg grows a variant; the interesting
/// discrimination for a driver is `Ended` vs. everything else, and that is stable.
#[non_exhaustive]
#[derive(Debug)]
pub enum Fatal {
    /// A ceiling fired, the operator ran `agg stop`, or Ctrl-C landed — the enforcement path.
    /// Carries the outcome the run is latched to; `RunOutcome::exit_code` maps it to a process code.
    Ended(RunOutcome),
    /// The driver's (or agg's) own `?` on a file operation. Kept distinct from [`Fatal::Other`] so a
    /// driver can match `ErrorKind` without downcasting through `anyhow`.
    Io(std::io::Error),
    /// Everything the pipeline raises — `step_once` and friends return `anyhow::Result`.
    Other(anyhow::Error),
}

impl From<std::io::Error> for Fatal {
    fn from(e: std::io::Error) -> Self {
        Fatal::Io(e)
    }
}

impl From<anyhow::Error> for Fatal {
    fn from(e: anyhow::Error) -> Self {
        Fatal::Other(e)
    }
}

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fatal::Ended(o) => write!(f, "run ended: {o:?} (exit {})", o.exit_code()),
            Fatal::Io(e) => write!(f, "{e}"),
            // `{:#}` is anyhow's single-line context chain — the whole "why", not just the tip.
            Fatal::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for Fatal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Fatal::Ended(_) => None,
            Fatal::Io(e) => Some(e),
            Fatal::Other(e) => Some(&**e),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Step results
// ---------------------------------------------------------------------------------------------

/// Where a step's work ENDED UP. Not a bool, because the driver's next `if` usually has to tell
/// "staged on the span, waiting for a gate" from "nothing happened at all".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Landing {
    /// merged into the base branch.
    Base,
    /// committed on the session branch and STAGED on the open span — it lands when `gate()` says so.
    /// On the driver path this is the normal case: `step()` always stages.
    Span,
    /// the work was produced and then discarded by policy.
    RolledBack,
    /// no commits — the worker changed nothing, or the session was vetoed.
    Nothing,
}

/// What one `agg.step(&s)?` did.
///
/// ⚠ **Not `#[must_use]`, on purpose** (BUILD.md §3.3): most `step()` call sites discard the value,
/// and the shipped tree has zero explicit `#[must_use]`. A lint that fires on the common case
/// teaches driver authors to write `let _ =`, which is worse than no lint.
///
/// Serde-derived because Phase 1's ledger (`calls.jsonl`) records it verbatim and replays it on
/// fast-forward — `session`, `tokens` and `cost` are what stop a resume from laundering the
/// ceilings, so they are carried AS OF THIS STEP, not recomputed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutcome {
    /// the step's name, as the driver spelled it.
    pub step: String,
    /// the run's session number after this step.
    pub session: u32,
    pub landed: Landing,
    /// the RUN-SET verdicts (the ceiling detectors), in order.
    ///
    /// ⚠ **Empty on the driver path**, and that is correct rather than a gap: nothing is declared to
    /// agg here (no `abort_if`/`notify_if`/`invariants`), so there is no run-set to run. The judges
    /// a driver asks for are LAZY — they live in `agg.judge(..)`'s per-step cache, not here.
    pub verdicts: Vec<(String, Verdict)>,
    /// cumulative output tokens spent by the run as of this step.
    pub tokens: u64,
    /// cumulative dollars spent by the run as of this step.
    pub cost: f64,
    /// wall-clock seconds this step took.
    pub secs: u64,
    /// the agent process's exit code.
    pub exit: i32,
}

// ---------------------------------------------------------------------------------------------
// Gate results
// ---------------------------------------------------------------------------------------------

/// What `agg.gate()` did with the open span.
///
/// ⚠ Not `#[must_use]`, for the same reason as [`StepOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateOutcome {
    /// the span merged into base.
    Kept,
    /// the span was discarded by the [`OnRegression`] policy; base is unchanged.
    RolledBack,
    /// there was nothing to gate — no open span, or a span with no commits. No ref was touched.
    Nothing,
    /// git could not stage the span. **Distinct from [`GateOutcome::RolledBack`]**: a merge conflict
    /// is not a policy decision and not a discard, and in the cases that keep the span tip the
    /// driver (or the operator) can gate again after fixing it.
    Failed(GateFailure),
}

/// Why a gate could not complete. Each variant maps 1:1 to a `git::stage_session` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateFailure {
    /// the merge conflicted and was aborted. The span tip is **KEPT** and base is untouched, so the
    /// span is still gateable once the operator resolves it.
    Conflict,
    /// the base branch could not be checked out. Nothing was staged; the span tip is KEPT.
    CheckoutFailed,
    /// a veto file was present. The span tip was DELETED and the span is cleared.
    Vetoed,
}

// ---------------------------------------------------------------------------------------------
// Run configuration
// ---------------------------------------------------------------------------------------------

/// What a regression across a span MEANS to this project — the policy `gate()` applies.
///
/// A regression is "a judge that was met as of the last gate is now unmet, and did not error"
/// (BUILD.md §3.5). That rule is only sound because **a judge's `met` means GOOD** on this path; an
/// inverted detector must be inverted before it is used as a driver judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnRegression {
    /// land the span anyway and record the regression. Choose this for exploratory work, where
    /// throwing away a night's progress over one late regression is the worse failure.
    Annotate,
    /// discard the whole span. The default, matching the shipped YAML default
    /// (`sequence.gate_regressions: true`).
    #[default]
    Rollback,
}

/// Run-level options that must be known at `Agg::open_with` time, before any config is applied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Opts {
    /// fast-forward through the calls a previous process already completed, from `calls.jsonl`.
    ///
    /// OPT-IN: agg does not decide on its own that a driver's re-run is a continuation, because it
    /// cannot know whether the driver's source changed underneath the ledger.
    pub resume: bool,
}

// ---------------------------------------------------------------------------------------------
// Agent + effort
// ---------------------------------------------------------------------------------------------

/// Which agent CLI drives a step.
///
/// The three unit variants are exactly `backend::KNOWN`; naming them as an enum instead of a string
/// is the one place where the Rust path is strictly better than YAML, because a typo is a compile
/// error rather than a run that refuses at preflight.
#[derive(Clone, Default)]
pub enum Agent {
    #[default]
    Claude,
    Codex,
    Copilot,
    /// A backend the driver implements itself.
    ///
    /// ⚠ This is **the documented exception** to "`Step` ≅ `ResolvedStep`, one struct with two
    /// constructors": a step carrying an `Arc<dyn AgentBackend>` cannot be `Deserialize`, so YAML
    /// can never express it. The Rust builder is a superset of the YAML shape, not a mirror of it.
    ///
    /// ⛔ It is still a *subprocess* backend. An in-process agent arm was rejected: `isolation:`
    /// would silently mean nothing for it, because there is no process for the OS wrapper to wrap.
    /// Ship a shim binary instead.
    Custom(Arc<dyn AgentBackend>),
}

impl Agent {
    /// The `agent:` name this resolves to — what `backend::for_name` would be asked for, and what
    /// the banner and per-agent accounting print.
    pub fn name(&self) -> &str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Copilot => "copilot",
            Agent::Custom(b) => b.name(),
        }
    }
}

// Hand-written because `dyn AgentBackend` is not `Debug` — and requiring it would be a trait-wide
// tax on every backend author for one line of formatting.
impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Agent::Custom(b) => write!(f, "Custom({})", b.name()),
            other => f.write_str(other.name()),
        }
    }
}

/// Thinking effort for a step.
///
/// The vocabulary is `low|medium|high|xhigh|max` — Claude's and Copilot's shared scale
/// (`backend/mod.rs`). Codex takes no effort flag at all, which is why [`Effort::Default`] exists
/// and why the field on a step is an `Effort` rather than an `Option<Effort>`: "I did not ask for an
/// effort" is a real, nameable choice, and agg's blanket `max` would otherwise be a demand no Codex
/// run could satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Effort {
    /// let the backend pick — for Codex that means passing no effort flag whatsoever.
    #[default]
    Default,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// The literal agg.yaml/CLI spelling, or `None` for [`Effort::Default`] (= defer to the
    /// backend). Mirrors `ResolvedStep::effort`'s `Option<String>`, so the two paths agree.
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Effort::Default => None,
            Effort::Low => Some("low"),
            Effort::Medium => Some("medium"),
            Effort::High => Some("high"),
            Effort::Xhigh => Some("xhigh"),
            Effort::Max => Some("max"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vocabulary is load-bearing: `capability::check` refuses a run whose effort string the
    /// backend does not know, so a typo here would be a runtime refusal, not a compile error.
    #[test]
    fn effort_spells_the_backend_vocabulary() {
        let all = [Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max];
        let spelled: Vec<_> = all.iter().map(|e| e.as_str().unwrap()).collect();
        assert_eq!(spelled, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(Effort::default().as_str(), None, "Default must defer to the backend, not spell a level");
    }

    /// `Agent` must name exactly the backends `backend::for_name` resolves, or a driver could
    /// construct a step that refuses at preflight.
    #[test]
    fn agent_names_match_the_known_backends() {
        let named = [Agent::Claude, Agent::Codex, Agent::Copilot];
        let names: Vec<_> = named.iter().map(|a| a.name()).collect();
        assert_eq!(names, crate::backend::KNOWN);
        for a in &named {
            crate::backend::for_name(a.name()).expect("every Agent variant resolves to a backend");
        }
    }

    /// `?` from a driver's own file I/O must land in `Fatal` without a manual `map_err`, and the
    /// enforcement path must stay distinguishable from it.
    #[test]
    fn fatal_absorbs_both_error_kinds_and_keeps_ended_distinct() {
        fn io() -> Result<(), Fatal> {
            std::fs::read_to_string("/definitely/not/here")?;
            Ok(())
        }
        assert!(matches!(io().unwrap_err(), Fatal::Io(_)));

        let other: Fatal = anyhow::anyhow!("pipeline blew up").into();
        assert_eq!(other.to_string(), "pipeline blew up");

        let ended = Fatal::Ended(RunOutcome::MaxSessions);
        assert!(ended.to_string().contains("exit 4"), "Display names the exit code: {ended}");
    }

    /// The shipped YAML default is `gate_regressions: true`, i.e. discard a regressed span. The two
    /// paths must not disagree about what a project gets when it says nothing.
    #[test]
    fn on_regression_defaults_to_the_shipped_yaml_behaviour() {
        assert_eq!(OnRegression::default(), OnRegression::Rollback);
    }
}
