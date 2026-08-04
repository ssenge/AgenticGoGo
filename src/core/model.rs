//! Core data model: judges, verdicts, lifecycle states.
//!
//! A [`Judge`] is a goal made executable (§7.1 — "a judge IS a goal"): resolved by NAME from disk
//! (§5.1), it carries its run-set membership and the [`Lifecycle`] derived from its verdict
//! history, so we can detect `Regressed` — a judge that was met and is now unmet. A [`Verdict`] is
//! what a judge returns each step.

use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;

/// Derived lifecycle state of a judge across steps.
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

/// Uniform judge output — the judge contract. Every judge (script or LLM) prints
/// exactly this JSON to stdout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub met: bool,
    /// numeric measure the judge emitted: a count, a percent, or 0/1. `None` = the judge emitted NO
    /// number (a binary goal says only `met`; a broken judge emits nothing usable). `Some(0.0)` is a
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
    /// set when the judge itself failed to run (vs. a clean "not met")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn one() -> f64 {
    1.0
}

impl Verdict {
    /// Construct the verdict for a judge that failed to execute.
    pub fn failed(error: impl Into<String>) -> Self {
        let error = error.into();
        Verdict {
            met: false,
            // a broken judge emitted no number — not a measured 0.
            value: None,
            max: None,
            target: 1.0,
            rationale: format!("judge failed: {error}"),
            evidence: vec![],
            error: Some(error),
        }
    }

    /// A BINARY verdict: `met` and nothing else. `value` stays `None` — a binary judge emits no
    /// number, and flattening that to `0` is a bug agg has already shipped once (`verdicts.rs` is
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
        }
    }

    /// A MEASURED verdict: `value` out of `max`.
    ///
    /// `met` is `value >= max` — a cardinal goal is met when the count reaches its denominator.
    /// (BUILD.md §0.1 gives the signature and not the rule; this is the reading that makes
    /// `scored(7.0, 10.0)` and `scored(10.0, 10.0)` mean the obvious things.) A judge whose
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
    // reads a verdict inline — `if agg.judge(&x).met()` — where a field access on a temporary reads
    // worse, and because `value_or` has no field twin.

    /// Did the judge say GOOD? On the driver path `met` ALWAYS means good — `gate()`'s regression
    /// rule is "was met, now unmet", which only means "worse" under that convention. An inverted
    /// detector (the shipped `stalled`, met-when-bad) must be inverted before it is used here.
    pub fn met(&self) -> bool {
        self.met
    }

    /// The measured number, or `None` for a binary judge. ⛔ NEVER a fabricated `0` — `Some(0.0)` is
    /// a real, measured zero and the two must stay distinguishable. Compare through [`Self::value_or`].
    pub fn value(&self) -> Option<f64> {
        self.value
    }

    /// The ONE flattening helper. `d` is the DRIVER's stated fallback, at the call site, instead of
    /// a fabrication buried in the library: `if agg.judge(&survey).value_or(0.0) >= 85.0 { … }`.
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

    /// Set when the judge failed to RUN. A judge with an `error` has not said "not met" — it has
    /// said "I could not grade this", which is why `gate()` excludes it from the regression set and
    /// [`Judge::apply`] leaves the lifecycle untouched.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// What a [`Judge::native`] closure is handed.
///
/// ⛔ **The clock, randomness, the network and the environment are DELIBERATELY EXCLUDED, and this
/// type definition is where that decision lives.** A native judge that reads the wall clock returns
/// a different verdict on replay, and Phase 1's fast-forward would then silently diverge from the
/// run it claims to reproduce. `JudgeCtx` is a pure function of committed repo state plus agg's own
/// ledger. A judge that genuinely needs the clock is a [`Judge::script`], where the impurity lives
/// in a separate file a reviewer can see.
///
/// ponytail: the ctx is EMPTY until the facade lands (BUILD.md §3.7, commit 6), which adds
/// `met`/`value`/`verdict`/`previous`/`history`/`session`/`step`/`dir`/`read`/`diff` and the two
/// construction sites. The lifetime is here from day one on purpose — the ctx BORROWS the verdict
/// source it consults, and adding the parameter later would change every native closure's signature.
pub struct JudgeCtx<'a> {
    _borrow: PhantomData<&'a ()>,
}

/// A [`JudgeKind::Native`] body.
///
/// `Arc` rather than `Box` because [`Judge`] derives `Clone`, and that derive is load-bearing —
/// `snapshot_goal_state`/`restore_goal_state` clone the whole judge set every gate. `Send + Sync`
/// pre-pays parallel judge evaluation; it costs a closure author nothing today.
pub type NativeFn = Arc<dyn Fn(&JudgeCtx<'_>) -> Verdict + Send + Sync>;

/// How a resolved judge runs. On the YAML path the kind is decided by the FILE EXTENSION at
/// resolution time (§5.1): `.sh` ⇒ Script, `.md` ⇒ Llm. There is no `kind:` key and no registry —
/// the old serde-tagged `JudgeSpec` enum is gone. A Rust driver names the kind directly with
/// [`Judge::script`] / [`Judge::rubric`] / [`Judge::native`].
///
/// ⚠ `Debug` is HAND-WRITTEN because [`JudgeKind::Native`] holds an `Arc<dyn Fn>`, which is not
/// `Debug` — and requiring it would be a tax on every closure a driver writes. [`Judge`]'s own
/// derives then still compile.
#[derive(Clone)]
pub enum JudgeKind {
    /// a script whose stdout is the verdict JSON. `path` is the resolved file.
    Script { path: PathBuf },
    /// an LLM rubric ⇒ a tools-off call on the RULER. `path` is the resolved rubric file; `inputs`
    /// come from ITS OWN yaml frontmatter (§5.1), read at resolution time — no registry.
    Llm { path: PathBuf, inputs: Vec<String> },
    /// a Rust closure over a [`JudgeCtx`] — DRIVER-ONLY, `judges::resolve` never produces one.
    ///
    /// Justified on speed (no subprocess, no fork per judge) and on being compiler-checked.
    /// ⛔ NEVER on security: the isolation carve-out already protects script judges, and a script
    /// judge can be given context via env, so "compiled in, therefore unrewritable" is not a
    /// property `native` uniquely buys at the level that matters.
    Native { f: NativeFn },
}

impl std::fmt::Debug for JudgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JudgeKind::Script { path } => f.debug_struct("Script").field("path", path).finish(),
            JudgeKind::Llm { path, inputs } => {
                f.debug_struct("Llm").field("path", path).field("inputs", inputs).finish()
            }
            JudgeKind::Native { .. } => f.debug_struct("Native").finish_non_exhaustive(),
        }
    }
}

impl JudgeKind {
    /// A short display tag for the scoreboard/dashboard.
    pub fn tag(&self) -> &'static str {
        match self {
            JudgeKind::Script { .. } => "script",
            JudgeKind::Llm { .. } => "llm",
            JudgeKind::Native { .. } => "native",
        }
    }
}

/// A judge — a goal, made executable. Resolved by NAME from disk (§5.1); it carries its DoD-set
/// membership and its runtime lifecycle.
#[derive(Debug, Clone)]
pub struct Judge {
    /// the judge's NAME — the filename stem it was resolved from, and the id conditions reference.
    pub name: String,
    /// how it runs — decided by the resolved file's extension (§5.1).
    pub kind: JudgeKind,
    /// named in `sequence.invariants` — must STAY met.
    pub invariant: bool,
    /// in the DoD-set (`done_if` ∪ `invariants`) — the set the AGGREGATES range over (§5.3). A
    /// judge named ONLY in an `if` condition (e.g. `stalled`) is in the run-set but NOT the DoD-set,
    /// so `all_goals` can't be blocked on "we got stuck".
    pub in_dod: bool,
    /// PER-JUDGE timeout override, in seconds. `None` = the run-level `judge.timeout` (300s), which
    /// is what every YAML-resolved judge uses — there is no per-judge `timeout:` key. It exists for
    /// the driver path, where a 40-minute load test and a 2-second build check are both ordinary and
    /// one run-level number cannot serve both. Meaningless for [`JudgeKind::Native`] (no subprocess
    /// to kill).
    pub timeout: Option<u64>,

    // ---- runtime state ----
    pub state: Lifecycle,
    pub last_verdict: Option<Verdict>,
    /// has this judge EVER been met during this run? (drives regression detection)
    pub ever_met: bool,
}

impl Judge {
    /// An LLM RUBRIC judge: the `.md` file IS the prompt, graded by the RULER. Costs a model call.
    ///
    /// `path_md` is taken verbatim and resolved against the project dir when the judge RUNS, not
    /// here — a driver constructs its judges above `Agg::open(dir)` (the builder chain is
    /// self-consuming, so it must), and a constructor therefore cannot know the project root. The
    /// rubric's own `inputs:` frontmatter is read at that same point, which is why `inputs` starts
    /// empty rather than being filled from a path this call cannot yet resolve.
    pub fn rubric(name: impl Into<String>, path_md: impl Into<PathBuf>) -> Judge {
        Judge::declared(name, JudgeKind::Llm { path: path_md.into(), inputs: vec![] })
    }

    /// A SCRIPT judge: any executable whose stdout is the verdict JSON. Cheap, deterministic, no
    /// model call. `path_sh` is resolved like [`Self::rubric`]'s.
    pub fn script(name: impl Into<String>, path_sh: impl Into<PathBuf>) -> Judge {
        Judge::declared(name, JudgeKind::Script { path: path_sh.into() })
    }

    /// A NATIVE judge: a Rust closure over a [`JudgeCtx`]. See [`JudgeKind::Native`] for what it is
    /// and is not justified on.
    pub fn native(
        name: impl Into<String>,
        f: impl Fn(&JudgeCtx<'_>) -> Verdict + Send + Sync + 'static,
    ) -> Judge {
        Judge::declared(name, JudgeKind::Native { f: Arc::new(f) })
    }

    /// Override the run-level `judge.timeout` for this judge alone — `.timeout(45 * 60)` for a load
    /// test the 300s default would kill.
    pub fn timeout(mut self, secs: u64) -> Judge {
        self.timeout = Some(secs);
        self
    }

    /// The shared body of the three constructors.
    ///
    /// `invariant`/`in_dod` are BOTH false, and that is correct rather than a gap: on the driver
    /// path nothing is declared to agg (no `abort_if`, no `notify_if`, no `invariants`, no
    /// `done_if`), so there is no run-set and no DoD-set for a judge to be a member of. Judges are
    /// lazy and the driver asks.
    fn declared(name: impl Into<String>, kind: JudgeKind) -> Judge {
        Judge {
            name: name.into(),
            kind,
            invariant: false,
            in_dod: false,
            timeout: None,
            state: Lifecycle::Pending,
            last_verdict: None,
            ever_met: false,
        }
    }

    /// Fold a fresh verdict into the judge's lifecycle state. Returns the new state.
    ///
    /// # A BROKEN JUDGE IS NOT A FAILING JUDGE
    /// A verdict carrying `error` (spawn failure, timeout, garbage stdout — every
    /// [`Verdict::failed`] path) is the judge saying *"I could not grade this"*, NOT *"this is not
    /// met"*. It therefore **never changes the lifecycle**: never a regression, never satisfies
    /// `done_if`, never marks a judge met. We record the verdict (its `rationale` carries the error
    /// text) and leave `state`/`ever_met` exactly as they were. `any_judge_error` (see `core::stop`)
    /// is the front-door signal, and `abort_if: … OR any_judge_error` the explicit policy.
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
            // a measured value above 0 = partial progress. `None` (a binary/numberless judge) is not
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

    /// The presentational goal-type, INFERRED from the verdict (§7.1): a judge that emitted no
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

    /// A binary judge emits NO number, and `value_or` is the one place the fallback becomes a
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

    /// The three driver constructors, and the one thing they must NOT do: join agg's run-set.
    #[test]
    fn a_driver_judge_is_in_no_declared_set() {
        let judges = [
            Judge::rubric("survey_good", "agg/judges/survey_good.md"),
            Judge::script("builds", "agg/judges/build.sh"),
            Judge::native("p99_ok", |_| Verdict::binary(true)),
        ];
        for j in &judges {
            assert!(!j.in_dod, "`{}` must not join the DoD-set — nothing is declared to agg here", j.name);
            assert!(!j.invariant, "`{}` must not become an invariant", j.name);
            assert_eq!(j.state, Lifecycle::Pending);
            assert_eq!(j.timeout, None, "absent = the run-level judge.timeout");
        }
        let tags: Vec<_> = judges.iter().map(|j| j.kind.tag()).collect();
        assert_eq!(tags, ["llm", "script", "native"]);
    }

    /// A per-judge timeout overrides the 300s run default — a 40-minute load test needs it.
    #[test]
    fn timeout_is_a_consuming_setter() {
        let load = Judge::script("load_ok", "agg/judges/loadtest.sh").timeout(45 * 60);
        assert_eq!(load.timeout, Some(2700));
    }

    /// `Judge` derives `Clone`, and that derive is load-bearing (snapshot/restore_goal_state clone
    /// the whole judge set every gate). A native judge's closure must survive it — which is why the
    /// body is an `Arc` and not a `Box`.
    #[test]
    fn a_native_judge_survives_the_clone_the_gate_depends_on() {
        let j = Judge::native("always", |_| Verdict::binary(true).with_rationale("yes"));
        let copy = j.clone();
        let JudgeKind::Native { f } = &copy.kind else { panic!("kind survives the clone") };
        // the closure is callable through the clone. `JudgeCtx` is empty until commit 6; a judge
        // that ignores it is exactly what this asserts on.
        let ctx = JudgeCtx { _borrow: PhantomData };
        let v = f(&ctx);
        assert!(v.met() && v.rationale() == "yes");
    }

    /// `JudgeKind`'s `Debug` is hand-written (an `Arc<dyn Fn>` has none). It must still print
    /// something useful for the two file-backed kinds — the dashboard and every `{:?}` in a log
    /// went through the derive before.
    #[test]
    fn the_hand_written_debug_still_names_the_paths() {
        let s = format!("{:?}", Judge::script("builds", "agg/judges/build.sh").kind);
        assert!(s.contains("Script") && s.contains("build.sh"), "got: {s}");
        let l = format!("{:?}", Judge::rubric("good", "agg/judges/good.md").kind);
        assert!(l.contains("Llm") && l.contains("good.md"), "got: {l}");
        let n = format!("{:?}", Judge::native("n", |_| Verdict::binary(true)).kind);
        assert!(n.contains("Native"), "got: {n}");
    }
}
