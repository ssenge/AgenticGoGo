//! Core data model: verifiers, verdicts, lifecycle states.
//!
//! A [`Verifier`] is a goal made executable (§7.1 — "a verifier IS a goal"): resolved by NAME from disk
//! (§5.1), it carries its run-set membership and the [`Lifecycle`] derived from its verdict
//! history, so we can detect `Regressed` — a verifier that was met and is now unmet. A [`Verdict`] is
//! what a verifier returns each step.

use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Derived lifecycle state of a verifier across steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// never evaluated, or evaluated with zero progress
    Pending,
    /// evaluated, partial progress, not met
    InProgress,
    /// currently meets target
    Met,
    /// was Met in a prior step, now unmet — first-class signal
    Regressed,
}

impl Lifecycle {
    /// A short glyph for the scoreboard.
    pub fn glyph(self) -> &'static str {
        match self {
            Lifecycle::Pending => "·",
            Lifecycle::InProgress => "◑",
            Lifecycle::Met => "✔",
            Lifecycle::Regressed => "⚠",
        }
    }
}

/// Uniform verifier output — the verifier contract. Every verifier (script or LLM) prints
/// exactly this JSON to stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub met: bool,
    /// numeric measure the verifier emitted: a count, a percent, or 0/1. `None` = the verifier emitted NO
    /// number (a binary goal says only `met`; a broken verifier emits nothing usable). `Some(0.0)` is a
    /// REAL, measured zero and is DISTINCT from absent — `coverage.value == 0` must tell the two
    /// apart (`core::stop`), and `verdicts.jsonl` records the difference faithfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    /// denominator for cardinal/percentage goals. Absent (`None`) for the same reasons as `value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// presentational only (draws the progress bar) — deliberately NOT readable in a condition.
    #[serde(default = "one")]
    pub target: f64,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// set when the verifier itself failed to run (vs. a clean "not met")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// the verifier's own measurements moved in DIFFERENT directions (several tools, one says better,
    /// one says worse), so a ratchet must not count this verdict as an improvement. Optional in the
    /// contract; absent = false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mixed: bool,
    /// met VACUOUSLY: there was nothing to measure (a `scope: changed` metric verifier whose session
    /// changed no file its analyzer applies to). It moves nothing at the gate — no improvement, no
    /// regression, and it does not replace the verifier's last landed reading. Absent = false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vacuous: bool,
    /// what the goal waits on lies in another step's result (a path or a step's name), so no repair of the
    /// verified work can meet it. Optional in the contract; absent = nothing blocks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<String>,
}

fn one() -> f64 {
    1.0
}

impl Verdict {
    /// Construct the verdict for a verifier that failed to execute.
    pub fn failed(error: impl Into<String>) -> Self {
        let error = error.into();
        Verdict {
            met: false,
            // a broken verifier emitted no number — not a measured 0.
            value: None,
            max: None,
            target: 1.0,
            rationale: format!("verifier failed: {error}"),
            evidence: vec![],
            error: Some(error),
            mixed: false,
            vacuous: false,
            blocked_by: None,
        }
    }

    /// A BINARY verdict: `met` and nothing else. `value` stays `None` — a binary verifier emits no
    /// number, and flattening that to `0` is a bug rlt has already shipped once (`verdicts.rs` is
    /// deliberately careful to keep absent absent).
    pub fn binary(met: bool) -> Self {
        Verdict {
            met,
            value: None,
            max: None,
            target: 1.0,
            rationale: String::new(),
            evidence: vec![],
            error: None,
            mixed: false,
            vacuous: false,
            blocked_by: None,
        }
    }

    /// A MEASURED verdict: `value` out of `max`.
    ///
    /// `met` is `value >= max` — a cardinal goal is met when the count reaches its denominator.
    /// (BUILD.md §0.1 gives the signature and not the rule; this is the reading that makes
    /// `scored(7.0, 10.0)` and `scored(10.0, 10.0)` mean the obvious things.) A verifier whose
    /// threshold is NOT its denominator says so explicitly and keeps the number:
    /// `Verdict::binary(pct >= 85.0).with_value(pct).with_max(100.0)`.
    pub fn scored(value: f64, max: f64) -> Self {
        Verdict { met: value >= max, value: Some(value), max: Some(max), ..Verdict::binary(false) }
    }

    // ---- CONSUMING setters, `with_`-prefixed ----
    //
    // ⛔ The prefix is not style. A reader `value(&self)` and a setter `value(self, f64)` are two
    // inherent methods with one name — E0592, Rust has no method overloading — so the pair does not
    // compile whatever their arities or receivers. The readers keep the short names because they are
    // the ones on every driver line.

    pub fn with_value(mut self, v: f64) -> Self {
        self.value = Some(v);
        self
    }
    pub fn with_max(mut self, m: f64) -> Self {
        self.max = Some(m);
        self
    }
    pub fn with_rationale(mut self, s: impl Into<String>) -> Self {
        self.rationale = s.into();
        self
    }
    pub fn with_evidence(mut self, e: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.evidence = e.into_iter().map(Into::into).collect();
        self
    }

    // ---- readers ----
    //
    // The fields are `pub` and stay `pub` (this crate has no facade); these exist because a driver
    // reads a verdict inline — `if rlt.verifier(&x).met()` — where a field access on a temporary reads
    // worse, and because `value_or` has no field twin.

    /// Did the verifier say GOOD? On the driver path `met` ALWAYS means good — `gate()`'s regression
    /// rule is "was met, now unmet", which only means "worse" under that convention. An inverted
    /// detector (the shipped `stalled`, met-when-bad) must be inverted before it is used here.
    pub fn met(&self) -> bool {
        self.met
    }

    /// The measured number, or `None` for a binary verifier. ⛔ NEVER a fabricated `0` — `Some(0.0)` is
    /// a real, measured zero and the two must stay distinguishable. Compare through [`Self::value_or`].
    pub fn value(&self) -> Option<f64> {
        self.value
    }

    /// The ONE flattening helper. `d` is the DRIVER's stated fallback, at the call site, instead of
    /// a fabrication buried in the library: `if rlt.verifier(&survey).value_or(0.0) >= 85.0 { … }`.
    pub fn value_or(&self, d: f64) -> f64 {
        self.value.unwrap_or(d)
    }

    pub fn max(&self) -> Option<f64> {
        self.max
    }
    pub fn rationale(&self) -> &str {
        &self.rationale
    }
    pub fn evidence(&self) -> &[String] {
        &self.evidence
    }

    /// Set when the verifier failed to RUN. A verifier with an `error` has not said "not met" — it has
    /// said "I could not grade this", which is why `gate()` excludes it from the regression set and
    /// [`Verifier::apply`] leaves the lifecycle untouched.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// What a [`Verifier::native`] closure is handed.
///
/// ⛔ **The clock, randomness, the network and the environment are DELIBERATELY EXCLUDED, and this
/// type definition is where that decision lives.** A native verifier that reads the wall clock returns
/// a different verdict on replay, and Phase 1's fast-forward would then silently diverge from the
/// run it claims to reproduce. `VerifierCtx` is a pure function of committed repo state plus rlt's own
/// ledger. A verifier that genuinely needs the clock is a [`Verifier::script`], where the impurity lives
/// in a separate file a reviewer can see.
///
/// # What it can reach
///
/// Committed repo state (`dir`, `read`, `diff`) and rlt's own ledger (`previous`, `history`), plus
/// the other verifiers — through the SAME lazy per-step cache `rlt.verifier()` uses, so asking for an
/// unconsulted verifier RUNS it, memoized, with its spend charged exactly as a direct ask would be.
/// That is the price of `met`/`verdict` returning non-`Option` values, and it is the right price:
/// the alternative is fabricating a verdict. Recursion (a verifier asking for itself, directly or
/// through a cycle) is REFUSED with a [`Verdict::failed`] naming the cycle.
pub struct VerifierCtx<'a> {
    source: &'a dyn VerifierSource,
    session: u32,
    step: &'a str,
    dir: &'a Path,
    /// captured EAGERLY at construction — see [`VerifierCtx::diff`].
    diff: String,
    /// the shared per-session verifier scratch — see [`VerifierCtx::scratch`].
    scratch: std::path::PathBuf,
    _borrow: PhantomData<&'a ()>,
}

/// "Give me the verdict for this verifier, running it if needed."
///
/// The ONE seam both [`VerifierCtx`] construction sites go through, so they build the ctx identically
/// while seeing different verdict stores: the facade's per-step `RefCell` cache on the Rust path,
/// and `Engine` state on the YAML path.
pub trait VerifierSource {
    fn verdict_for(&self, j: &Verifier) -> Verdict;
}

impl<'a> VerifierCtx<'a> {
    /// `diff` is the session's diff against the isolation base, captured by the CALLER before any
    /// verifier runs — see [`VerifierCtx::diff`] for why it cannot be lazy.
    pub fn new(
        source: &'a dyn VerifierSource,
        session: u32,
        step: &'a str,
        dir: &'a Path,
        diff: String,
        scratch: std::path::PathBuf,
    ) -> VerifierCtx<'a> {
        VerifierCtx { source, session, step, dir, diff, scratch, _borrow: PhantomData }
    }

    /// The directory verifiers may WRITE this session — the same `$RLT_VERIFIER_SCRATCH` a script verifier
    /// gets, so a native closure and a script can hand a measurement to each other.
    ///
    /// A native verifier is a Rust closure running IN-PROCESS, so nothing stops it writing wherever it
    /// likes; this exists so it does not have to want to. The project tree is read-only to every
    /// SCRIPT verifier (§2.5), and a native verifier that writes there anyway breaks the same guarantee
    /// silently instead of loudly — it is a bug either way, so point it here.
    ///
    /// Shared per session, not per verifier: `load_ok` writes `bench.json` here, `p99_ok` reads it.
    pub fn scratch(&self) -> &Path {
        &self.scratch
    }

    /// Did `j` say GOOD? Runs `j` if this step has not asked for it yet.
    pub fn met(&self, j: &Verifier) -> bool {
        self.verdict(j).met
    }

    /// `j`'s measured number, or `None` for a binary verifier. ⛔ NEVER a fabricated `0`.
    pub fn value(&self, j: &Verifier) -> Option<f64> {
        self.verdict(j).value
    }

    /// `j`'s whole verdict this step, from the shared lazy cache (running it on a miss).
    pub fn verdict(&self, j: &Verifier) -> Verdict {
        self.source.verdict_for(j)
    }

    /// `j`'s last MERGED verdict — `verdicts.jsonl`'s `merged`/`baseline` rows, i.e. the very
    /// baseline the regression gate measures against, so a native verifier and the gate cannot
    /// disagree about what "before" means. `None` = `j` has no landed row yet.
    pub fn previous(&self, j: &Verifier) -> Option<Verdict> {
        crate::core::verdicts::rows_for(self.dir, &j.name, true).last().map(|r| r.verdict())
    }

    /// `j`'s last `n` recorded verdicts, OLDEST FIRST — every outcome, not just the landed ones,
    /// because "the test count must never shrink" is a statement about the series rlt observed.
    pub fn history(&self, j: &Verifier, n: usize) -> Vec<Verdict> {
        let rows = crate::core::verdicts::rows_for(self.dir, &j.name, false);
        rows[rows.len().saturating_sub(n)..].iter().map(|r| r.verdict()).collect()
    }

    pub fn session(&self) -> u32 {
        self.session
    }

    /// The step whose session is being graded.
    pub fn step(&self) -> &str {
        self.step
    }

    pub fn dir(&self) -> &Path {
        self.dir
    }

    /// Read a project file, RELATIVE to the project dir — which is the whole reason this exists
    /// beside [`Self::dir`]: it is the one place that rule is enforced, and it is what makes a
    /// verifier relocatable.
    ///
    /// ponytail: no path-traversal check. CEILING: `read("../../etc/passwd")` resolves. Harmless
    /// today (the path is a literal in the driver's own compiled source, not worker-authored).
    /// UPGRADE PATH: reject `..` the first time a verifier reads a path the worker can influence.
    pub fn read(&self, rel: &str) -> anyhow::Result<String> {
        let p = self.dir.join(rel);
        std::fs::read_to_string(&p).map_err(|e| anyhow::anyhow!("reading {}: {e}", p.display()))
    }

    /// This session's diff against the isolation base, captured EAGERLY when the ctx was built.
    ///
    /// The `&str` return says so. It is not lazy on purpose: a lazy read would see the tree AFTER
    /// some other verifier's script had perturbed it, and determinism is this type's whole reason for
    /// its shape.
    pub fn diff(&self) -> &str {
        &self.diff
    }
}

/// A [`VerifierKind::Native`] body.
///
/// `Arc` rather than `Box` because [`Verifier`] derives `Clone`, and that derive is load-bearing —
/// `snapshot_goal_state`/`restore_goal_state` clone the whole verifier set every gate. `Send + Sync`
/// pre-pays parallel verifier evaluation; it costs a closure author nothing today.
pub type NativeFn = Arc<dyn Fn(&VerifierCtx<'_>) -> Verdict + Send + Sync>;

/// How a resolved verifier runs. On the YAML path the kind is decided by the FILE EXTENSION at
/// resolution time (§5.1): `.sh` ⇒ Script, `.md` ⇒ Llm. There is no `kind:` key and no registry —
/// the old serde-tagged `VerifierSpec` enum is gone. A Rust driver names the kind directly with
/// [`Verifier::script`] / [`Verifier::rubric`] / [`Verifier::native`].
///
/// ⚠ `Debug` is HAND-WRITTEN because [`VerifierKind::Native`] holds an `Arc<dyn Fn>`, which is not
/// `Debug` — and requiring it would be a tax on every closure a driver writes. [`Verifier`]'s own
/// derives then still compile.
#[derive(Clone)]
pub enum VerifierKind {
    /// a script whose stdout is the verdict JSON. `path` is the resolved file.
    Script { path: PathBuf },
    /// an LLM rubric ⇒ a tools-off call on the RULER. `path` is the resolved rubric file; `inputs`
    /// come from ITS OWN yaml frontmatter (§5.1), read at resolution time — no registry.
    Llm { path: PathBuf, inputs: Vec<String> },
    /// a Rust closure over a [`VerifierCtx`] — DRIVER-ONLY, `verifiers::resolve` never produces one.
    ///
    /// Justified on speed (no subprocess, no fork per verifier) and on being compiler-checked.
    /// ⛔ NEVER on security: the isolation carve-out already protects script verifiers, and a script
    /// verifier can be given context via env, so "compiled in, therefore unrewritable" is not a
    /// property `native` uniquely buys at the level that matters.
    Native { f: NativeFn },
    /// one number read from the session's shared `swe-metrics measure` report, held to a threshold
    /// ([`crate::metrics`]). YAML: a `verifiers.<name>.metric:` block, not a file.
    Metric(Box<crate::metrics::MetricVerifier>),
    /// swe-metrics' quality score of the session's shared report, scored against the incumbent's
    /// report ([`crate::metrics`]). YAML: a `verifiers.<name>.score:` block, not a file.
    Score(Box<crate::metrics::ScoreVerifier>),
    /// a command held to its exit code: met when it exits 0 ([`CommandVerifier`]). YAML: a
    /// `verifiers.<name>.command:` entry, not a file.
    Command(Box<CommandVerifier>),
}

/// A COMMAND verifier: a shell command run in the project (the verifier's working directory), met when it exits 0. The
/// last lines of its output are the rationale; `path` entries (relative to the project) go in front of `PATH`.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandVerifier {
    pub command: String,
    pub path: Vec<String>,
}

impl CommandVerifier {
    pub fn new(command: impl Into<String>) -> CommandVerifier {
        CommandVerifier { command: command.into(), path: vec![] }
    }

    /// Directories (relative to the project) put in front of `PATH` for the command.
    pub fn path(mut self, dirs: impl IntoIterator<Item = impl Into<String>>) -> CommandVerifier {
        self.path = dirs.into_iter().map(Into::into).collect();
        self
    }
}

impl std::fmt::Debug for VerifierKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifierKind::Script { path } => f.debug_struct("Script").field("path", path).finish(),
            VerifierKind::Llm { path, inputs } => {
                f.debug_struct("Llm").field("path", path).field("inputs", inputs).finish()
            }
            VerifierKind::Native { .. } => f.debug_struct("Native").finish_non_exhaustive(),
            VerifierKind::Metric(m) => f.debug_tuple("Metric").field(m).finish(),
            VerifierKind::Score(s) => f.debug_tuple("Score").field(s).finish(),
            VerifierKind::Command(c) => f.debug_tuple("Command").field(c).finish(),
        }
    }
}

impl VerifierKind {
    /// A short display tag for the scoreboard/dashboard.
    pub fn tag(&self) -> &'static str {
        match self {
            VerifierKind::Script { .. } => "script",
            VerifierKind::Llm { .. } => "llm",
            VerifierKind::Native { .. } => "native",
            VerifierKind::Metric(_) => "metric",
            VerifierKind::Score(_) => "score",
            VerifierKind::Command(_) => "command",
        }
    }
}

/// An rubric verifier's own ruler ([`Verifier::agent`], [`Verifier::model`], [`Verifier::effort`]); each `None` falls
/// back to the run's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VerifierRuler {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// A verifier — a goal, made executable. Resolved by NAME from disk (§5.1); it carries its DoD-set
/// membership and its runtime lifecycle.
#[derive(Debug, Clone)]
pub struct Verifier {
    /// the verifier's NAME — the filename stem it was resolved from, and the id conditions reference.
    pub name: String,
    /// how it runs — decided by the resolved file's extension (§5.1).
    pub kind: VerifierKind,
    /// named in `sequence.invariants` — must STAY met.
    pub invariant: bool,
    /// in the DoD-set (`done_if` ∪ `invariants`) — the set the AGGREGATES range over (§5.3). A
    /// verifier named ONLY in an `if` condition (e.g. `stalled`) is in the run-set but NOT the DoD-set,
    /// so `all_goals` can't be blocked on "we got stuck".
    pub in_dod: bool,
    /// PER-VERIFIER timeout override, in seconds. `None` = the run-level `verifier.timeout` (300s), which
    /// is what every YAML-resolved verifier uses — there is no per-verifier `timeout:` key. It exists for
    /// the driver path, where a 40-minute load test and a 2-second build check are both ordinary and
    /// one run-level number cannot serve both. Meaningless for [`VerifierKind::Native`] (no subprocess
    /// to kill).
    pub timeout: Option<u64>,
    /// PER-VERIFIER ruler for an rubric verifier: its own agent, model and effort. `None` = the run's ruler,
    /// which is what every YAML-resolved verifier uses. Set with [`Verifier::agent`] / [`Verifier::model`] /
    /// [`Verifier::effort`]; meaningless for every other kind.
    pub ruler: Option<VerifierRuler>,

    // ---- runtime state ----
    pub state: Lifecycle,
    pub last_verdict: Option<Verdict>,
    /// has this verifier EVER been met during this run? (drives regression detection)
    pub ever_met: bool,
}

impl Verifier {
    /// An LLM RUBRIC verifier: the `.md` file IS the prompt, graded by the RULER. Costs a model call.
    ///
    /// `path_md` is taken verbatim and resolved against the project dir when the verifier RUNS, not
    /// here — a driver constructs its verifiers above `Relentless::open(dir)` (the builder chain is
    /// self-consuming, so it must), and a constructor therefore cannot know the project root. The
    /// rubric's own `inputs:` frontmatter is read at that same point, which is why `inputs` starts
    /// empty rather than being filled from a path this call cannot yet resolve.
    pub fn rubric(name: impl Into<String>, path_md: impl Into<PathBuf>) -> Verifier {
        Verifier::declared(name, VerifierKind::Llm { path: path_md.into(), inputs: vec![] })
    }

    /// A SCRIPT verifier: any executable whose stdout is the verdict JSON. Cheap, deterministic, no
    /// model call. `path_sh` is resolved like [`Self::rubric`]'s.
    pub fn script(name: impl Into<String>, path_sh: impl Into<PathBuf>) -> Verifier {
        Verifier::declared(name, VerifierKind::Script { path: path_sh.into() })
    }

    /// A NATIVE verifier: a Rust closure over a [`VerifierCtx`]. See [`VerifierKind::Native`] for what it is
    /// and is not justified on.
    pub fn native(
        name: impl Into<String>,
        f: impl Fn(&VerifierCtx<'_>) -> Verdict + Send + Sync + 'static,
    ) -> Verifier {
        Verifier::declared(name, VerifierKind::Native { f: Arc::new(f) })
    }

    /// A METRIC verifier: one number from the session's shared `swe-metrics measure` report, held to
    /// the spec's threshold — `Verifier::metric("cc_p95", MetricVerifier::new("complexity.cyclomatic")
    /// .stat(Stat::P95).tool("lizard").max(10.0))`. Needs `swe-metrics` on `PATH`.
    pub fn metric(name: impl Into<String>, spec: crate::metrics::MetricVerifier) -> Verifier {
        Verifier::declared(name, VerifierKind::Metric(Box::new(spec)))
    }

    /// A SCORE verifier: swe-metrics' quality score (0–100) of the session's shared report —
    /// `Verifier::score("quality", ScoreVerifier::new("swe-default", "2").min(80.0))`.
    pub fn score(name: impl Into<String>, spec: crate::metrics::ScoreVerifier) -> Verifier {
        Verifier::declared(name, VerifierKind::Score(Box::new(spec)))
    }

    /// A COMMAND verifier: met when the shell command exits 0 —
    /// `Verifier::command("tests", CommandVerifier::new("pytest -q").path([".venv/bin"]))`. Its output's last lines are the
    /// rationale, so no wrapper script is needed.
    pub fn command(name: impl Into<String>, spec: CommandVerifier) -> Verifier {
        Verifier::declared(name, VerifierKind::Command(Box::new(spec)))
    }

    /// Override the run-level `verifier.timeout` for this verifier alone — `.timeout(45 * 60)` for a load
    /// test the 300s default would kill.
    pub fn timeout(mut self, secs: u64) -> Verifier {
        self.timeout = Some(secs);
        self
    }

    /// The AGENT this rubric verifier asks, instead of the run's ruler — a reviewer that is not the worker's
    /// agent. Its model is then that agent's default verifier model unless [`Verifier::model`] names one.
    /// `Agent::Custom` is refused when the verifier runs (the ruler is resolved by name).
    pub fn agent(mut self, agent: crate::driver::Agent) -> Verifier {
        self.ruler.get_or_insert_with(VerifierRuler::default).agent = Some(agent.name().to_string());
        self
    }

    /// The MODEL this rubric verifier asks, instead of the run's `verifier_model`.
    pub fn model(mut self, model: impl Into<String>) -> Verifier {
        self.ruler.get_or_insert_with(VerifierRuler::default).model = Some(model.into());
        self
    }

    /// The reasoning EFFORT of this rubric verifier's call (the run's ruler calls with none).
    pub fn effort(mut self, effort: crate::driver::Effort) -> Verifier {
        self.ruler.get_or_insert_with(VerifierRuler::default).effort = effort.as_str().map(String::from);
        self
    }

    /// The shared body of the three constructors.
    ///
    /// `invariant`/`in_dod` are BOTH false, and that is correct rather than a gap: on the driver
    /// path nothing is declared to rlt (no `abort_if`, no `notify_if`, no `invariants`, no
    /// `done_if`), so there is no run-set and no DoD-set for a verifier to be a member of. Verifiers are
    /// lazy and the driver asks.
    fn declared(name: impl Into<String>, kind: VerifierKind) -> Verifier {
        Verifier {
            name: name.into(),
            kind,
            invariant: false,
            in_dod: false,
            timeout: None,
            ruler: None,
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        }
    }

    /// Fold a fresh verdict into the verifier's lifecycle state. Returns the new state.
    ///
    /// # A BROKEN VERIFIER IS NOT A FAILING VERIFIER
    /// A verdict carrying `error` (spawn failure, timeout, garbage stdout — every
    /// [`Verdict::failed`] path) is the verifier saying *"I could not grade this"*, NOT *"this is not
    /// met"*. It therefore **never changes the lifecycle**: never a regression, never satisfies
    /// `done_if`, never marks a verifier met. We record the verdict (its `rationale` carries the error
    /// text) and leave `state`/`ever_met` exactly as they were. `any_verifier_error` (see `core::stop`)
    /// is the front-door signal, and `abort_if: … OR any_verifier_error` the explicit policy.
    pub fn apply(&mut self, verdict: Verdict) -> Lifecycle {
        if verdict.error.is_some() {
            self.last_verdict = Some(verdict);
            return self.state;
        }
        let met = verdict.met;
        let value = verdict.value;
        self.last_verdict = Some(verdict);
        self.state = if met {
            self.ever_met = true;
            Lifecycle::Met
        } else if self.ever_met {
            // was met before, now not -> regression
            Lifecycle::Regressed
        } else if value.is_some_and(|v| v > 0.0) {
            // a measured value above 0 = partial progress. `None` (a binary/numberless verifier) is not
            // progress — it stays Pending.
            Lifecycle::InProgress
        } else {
            Lifecycle::Pending
        };
        self.state
    }

    pub fn met(&self) -> bool {
        self.state == Lifecycle::Met
    }

    pub fn regressed(&self) -> bool {
        self.state == Lifecycle::Regressed
    }

    /// The presentational goal-type, INFERRED from the verdict (§7.1): a verifier that emitted no
    /// number is `binary`; one with a `max` reads as `cardinal`; else `percentage`.
    pub fn type_str(&self) -> &'static str {
        match self.last_verdict.as_ref() {
            Some(v) if v.value.is_none() => "binary",
            Some(v) if v.max.is_some() => "cardinal",
            Some(_) => "percentage",
            None => "binary",
        }
    }

    /// Compact one-line scoreboard representation.
    pub fn scoreboard_line(&self) -> String {
        let measure = match &self.last_verdict {
            None => "—".to_string(),
            Some(v) if v.value.is_none() => if v.met { "yes".into() } else { "no".into() },
            Some(v) => format!("{:.0}/{:.0}", v.value.unwrap_or(0.0), v.max.unwrap_or(v.target)),
        };
        format!(
            "{} {:<18} {:<10} {}",
            self.state.glyph(),
            self.name,
            self.kind.tag(),
            measure
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⛔ The `with_` prefix is what makes this file COMPILE. A reader `value(&self)` beside a
    /// setter `value(self, f64)` is E0592 — Rust has no method overloading — so this test exists to
    /// pin the pair down: both names are used, at both arities, in one expression.
    #[test]
    fn the_readers_and_the_with_setters_coexist() {
        let v = Verdict::binary(true).with_value(4.2).with_max(10.0).with_rationale("p99 4.2ms");
        assert!(v.met());
        assert_eq!(v.value(), Some(4.2));
        assert_eq!(v.max(), Some(10.0));
        assert_eq!(v.rationale(), "p99 4.2ms");
        assert!(v.evidence().is_empty());
        assert!(v.error().is_none());
    }

    /// A binary verifier emits NO number, and `value_or` is the one place the fallback becomes a
    /// visible decision. `Some(0.0)` is a measured zero and must stay distinct from absent.
    #[test]
    fn a_binary_verdict_has_no_number_and_a_measured_zero_is_not_absent() {
        let bare = Verdict::binary(false);
        assert_eq!(bare.value(), None, "never a fabricated 0");
        assert_eq!(bare.value_or(85.0), 85.0, "the default is the caller's, not the library's");

        let zero = Verdict::binary(false).with_value(0.0);
        assert_eq!(zero.value(), Some(0.0));
        assert_eq!(zero.value_or(85.0), 0.0, "a MEASURED zero wins over the fallback");
    }

    /// `scored(value, max)` means "met when the count reaches its denominator".
    #[test]
    fn scored_is_met_when_the_value_reaches_its_max() {
        assert!(!Verdict::scored(7.0, 10.0).met());
        assert!(Verdict::scored(10.0, 10.0).met());
        assert!(Verdict::scored(11.0, 10.0).met());
        let v = Verdict::scored(7.0, 10.0);
        assert_eq!((v.value(), v.max()), (Some(7.0), Some(10.0)));
        assert!(v.error().is_none(), "a scored verdict is a graded one, not a broken one");
    }

    /// `failed` is the shipped constructor and must keep meaning "I could not grade this".
    #[test]
    fn a_failed_verdict_still_carries_its_error_and_no_number() {
        let v = Verdict::failed("spawn: no such file");
        assert!(!v.met());
        assert_eq!(v.value(), None);
        assert_eq!(v.error(), Some("spawn: no such file"));
    }

    /// The three driver constructors, and the one thing they must NOT do: join rlt's run-set.
    #[test]
    fn a_driver_verifier_is_in_no_declared_set() {
        let verifiers = [
            Verifier::rubric("survey_good", "rlt/verifiers/survey_good.md"),
            Verifier::script("builds", "rlt/verifiers/build.sh"),
            Verifier::native("p99_ok", |_| Verdict::binary(true)),
        ];
        for j in &verifiers {
            assert!(!j.in_dod, "`{}` must not join the DoD-set — nothing is declared to rlt here", j.name);
            assert!(!j.invariant, "`{}` must not become an invariant", j.name);
            assert_eq!(j.state, Lifecycle::Pending);
            assert_eq!(j.timeout, None, "absent = the run-level verifier.timeout");
        }
        let tags: Vec<_> = verifiers.iter().map(|j| j.kind.tag()).collect();
        assert_eq!(tags, ["llm", "script", "native"]);
    }

    /// A per-verifier timeout overrides the 300s run default — a 40-minute load test needs it.
    #[test]
    fn timeout_is_a_consuming_setter() {
        let load = Verifier::script("load_ok", "rlt/verifiers/loadtest.sh").timeout(45 * 60);
        assert_eq!(load.timeout, Some(2700));
    }

    /// `Verifier` derives `Clone`, and that derive is load-bearing (snapshot/restore_goal_state clone
    /// the whole verifier set every gate). A native verifier's closure must survive it — which is why the
    /// body is an `Arc` and not a `Box`.
    #[test]
    fn a_native_verifier_survives_the_clone_the_gate_depends_on() {
        let j = Verifier::native("always", |_| Verdict::binary(true).with_rationale("yes"));
        let copy = j.clone();
        let VerifierKind::Native { f } = &copy.kind else { panic!("kind survives the clone") };
        // the closure is callable through the clone.
        let src = FixedSource(Verdict::binary(false));
        let ctx = VerifierCtx::new(&src, 1, "worker", Path::new("."), String::new(), std::env::temp_dir());
        let v = f(&ctx);
        assert!(v.met() && v.rationale() == "yes");
    }

    /// A verdict store that answers every verifier the same way — enough to build a `VerifierCtx`.
    struct FixedSource(Verdict);
    impl VerifierSource for FixedSource {
        fn verdict_for(&self, _j: &Verifier) -> Verdict {
            self.0.clone()
        }
    }

    /// The ctx's ten readers, and the two that are easy to get wrong: `value` never fabricates a
    /// `0`, and `diff` is the string the CALLER captured, not a fresh read.
    #[test]
    fn the_ctx_reads_its_source_the_ledger_and_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("VERSION"), "1.2.3").unwrap();
        let src = FixedSource(Verdict::binary(true).with_value(7.0));
        let ctx =
            VerifierCtx::new(&src, 4, "implement", tmp.path(), "diff --git a/x".into(), std::env::temp_dir());
        let other = Verifier::script("builds", "rlt/verifiers/build.sh");

        assert!(ctx.met(&other));
        assert_eq!(ctx.value(&other), Some(7.0));
        assert_eq!(ctx.verdict(&other).value(), Some(7.0));
        assert_eq!(ctx.session(), 4);
        assert_eq!(ctx.step(), "implement");
        assert_eq!(ctx.dir(), tmp.path());
        assert_eq!(ctx.read("VERSION").unwrap(), "1.2.3");
        assert!(ctx.read("nope.txt").is_err(), "a missing file is an Err, never an empty string");
        assert_eq!(ctx.diff(), "diff --git a/x");

        // no ledger yet ⇒ honestly no history, never a crash.
        assert!(ctx.previous(&other).is_none());
        assert!(ctx.history(&other, 5).is_empty());

        // …and a binary verdict still reports NO number through the ctx.
        let bare = FixedSource(Verdict::binary(true));
        let ctx = VerifierCtx::new(&bare, 1, "s", tmp.path(), String::new(), std::env::temp_dir());
        assert_eq!(ctx.value(&other), None, "never a fabricated 0");
    }

    /// `previous` is the last LANDED verdict and `history` is every row — the distinction the gate
    /// depends on. A rolled-back row describes code that does not exist, so it must not be "before".
    #[test]
    fn previous_reads_landed_rows_only_while_history_reads_them_all() {
        use crate::core::verdicts::{append, Outcome};
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        append(d, None, "baseline", &[("cov".into(), Verdict::scored(10.0, 100.0))], Outcome::Baseline).unwrap();
        append(d, Some(1), "w", &[("cov".into(), Verdict::scored(50.0, 100.0))], Outcome::Merged).unwrap();
        append(d, Some(2), "w", &[("cov".into(), Verdict::scored(20.0, 100.0))], Outcome::RolledBack).unwrap();

        let src = FixedSource(Verdict::binary(false));
        let ctx = VerifierCtx::new(&src, 3, "w", d, String::new(), std::env::temp_dir());
        let cov = Verifier::script("cov", "rlt/verifiers/cov.sh");

        assert_eq!(ctx.previous(&cov).and_then(|v| v.value()), Some(50.0), "the rolled-back 20 never landed");
        let series: Vec<_> = ctx.history(&cov, 10).iter().filter_map(|v| v.value()).collect();
        assert_eq!(series, [10.0, 50.0, 20.0], "history is every row, OLDEST first — the series `no_shrink` reads");
        assert_eq!(ctx.history(&cov, 2).len(), 2, "…and `n` takes the most recent");
    }

    /// `VerifierKind`'s `Debug` is hand-written (an `Arc<dyn Fn>` has none). It must still print
    /// something useful for the two file-backed kinds — the dashboard and every `{:?}` in a log
    /// went through the derive before.
    #[test]
    fn the_hand_written_debug_still_names_the_paths() {
        let s = format!("{:?}", Verifier::script("builds", "rlt/verifiers/build.sh").kind);
        assert!(s.contains("Script") && s.contains("build.sh"), "got: {s}");
        let l = format!("{:?}", Verifier::rubric("good", "rlt/verifiers/good.md").kind);
        assert!(l.contains("Llm") && l.contains("good.md"), "got: {l}");
        let n = format!("{:?}", Verifier::native("n", |_| Verdict::binary(true)).kind);
        assert!(n.contains("Native"), "got: {n}");
    }
}
