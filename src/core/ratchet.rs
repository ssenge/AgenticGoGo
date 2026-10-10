//! The numeric RATCHET and the gate's own history — `rollbacks`, `consecutive_rollbacks`,
//! `no_improvements`, `consecutive_no_improvements`, `oscillating`, `ratchet_regressed`,
//! `<judge>.ref`.
//!
//! # Why these are process-internal terms, not judges
//!
//! `stalled` is a judge that reads `verdicts.jsonl`, and that file sits in the worker's writable cwd
//! under `isolation: none` — a PROTOCOL boundary, not a permission one. Everything here is computed
//! from what the GATE decided, in the loop's own memory, and only PERSISTED (to `agg/private/`) so a
//! resume can pick it up. So, like `over_iterations`, these terms are unfakeable on every tier and
//! may sit in `abort_if`.
//!
//! # The ratchet in one sentence
//!
//! A ratcheted judge's `value` may not get worse than it was at the last KEPT gate, by more than its
//! tolerance. Before the first keep, the reference is the configured seed (`baseline:`) or, without
//! one, the pre-session baseline pass. A regression beyond tolerance is a gate regression: the
//! session is rolled back under `gate_regressions: true` (the default).

use crate::core::model::Verdict;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Which way is BETTER for a ratcheted judge's `value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// higher is better (coverage, mutation score).
    Up,
    /// lower is better (complexity, duplication, latency).
    Down,
}

/// One judge's ratchet: `sequence.ratchet.<judge>` in YAML, [`crate::driver::Agg::ratchet`] in Rust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ratchet {
    /// REQUIRED — there is no sensible default for which way is better.
    pub direction: Direction,
    /// how much worse than the reference is still accepted, in the judge's own units. Default 0.
    #[serde(default)]
    pub tolerance: f64,
    /// the same allowance as a percentage of the reference. With both set, the LARGER one applies.
    #[serde(default)]
    pub tolerance_pct: Option<f64>,
    /// the seeded reference for session 1. Absent ⇒ the baseline pass measures it.
    #[serde(default)]
    pub baseline: Option<f64>,
}

impl Ratchet {
    /// Higher is better, with an absolute `tolerance`.
    pub fn up(tolerance: f64) -> Ratchet {
        Ratchet { direction: Direction::Up, tolerance, tolerance_pct: None, baseline: None }
    }
    /// Lower is better, with an absolute `tolerance`.
    pub fn down(tolerance: f64) -> Ratchet {
        Ratchet { direction: Direction::Down, tolerance, tolerance_pct: None, baseline: None }
    }
    /// Add a percentage allowance (of the reference); the larger allowance wins.
    pub fn tolerance_pct(mut self, pct: f64) -> Ratchet {
        self.tolerance_pct = Some(pct);
        self
    }
    /// Seed the reference for session 1.
    pub fn baseline(mut self, v: f64) -> Ratchet {
        self.baseline = Some(v);
        self
    }

    /// Refuse a ratchet that cannot mean anything: a negative or non-finite allowance, a NaN seed.
    pub fn validate(&self, judge: &str) -> Result<()> {
        let bad = |what: &str, v: f64| v.is_nan() || v.is_infinite() || (what != "baseline" && v < 0.0);
        if bad("tolerance", self.tolerance) {
            bail!("ratchet `{judge}`: `tolerance` must be a finite number >= 0, got {}", self.tolerance);
        }
        if let Some(p) = self.tolerance_pct.filter(|p| bad("tolerance_pct", *p)) {
            bail!("ratchet `{judge}`: `tolerance_pct` must be a finite number >= 0, got {p}");
        }
        if let Some(b) = self.baseline.filter(|b| bad("baseline", *b)) {
            bail!("ratchet `{judge}`: `baseline` must be a finite number, got {b}");
        }
        Ok(())
    }

    /// The accepted worsening against `reference`: the larger of the absolute and the relative one.
    pub fn allowance(&self, reference: f64) -> f64 {
        let pct = self.tolerance_pct.map(|p| reference.abs() * p / 100.0).unwrap_or(0.0);
        self.tolerance.max(pct)
    }

    /// Did `now` move the WRONG way from `reference` by more than the allowance?
    ///
    /// The epsilon is float noise, not policy: `0.1 + 0.2` against a reference of `0.3` with zero
    /// tolerance must not read as a regression.
    pub fn regressed(&self, reference: f64, now: f64) -> bool {
        let worse = match self.direction {
            Direction::Up => reference - now,
            Direction::Down => now - reference,
        };
        worse > self.allowance(reference) + 1e-9 * reference.abs().max(1.0)
    }

    /// Did `now` move the GOOD way from `reference` by more than the allowance? The mirror of
    /// [`Self::regressed`] — what `merge_requires_improvement` counts as a ratcheted improvement, so
    /// noise inside the tolerance is no more an improvement than it is a regression.
    pub fn improved(&self, reference: f64, now: f64) -> bool {
        let better = match self.direction {
            Direction::Up => now - reference,
            Direction::Down => reference - now,
        };
        better > self.allowance(reference) + 1e-9 * reference.abs().max(1.0)
    }
}

/// One ratchet regression, as the gate recorded it: the judge, the reference it was held to, and
/// what it measured. `to: None` = the judge errored or emitted no number — it could not show that it
/// is no worse, and a ratchet never passes on an unmeasured value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RatchetRegression {
    pub judge: String,
    pub from: Option<f64>,
    pub to: Option<f64>,
}

/// What one gate did with its span — the input to [`Gate::record`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// the span merged onto base.
    Kept,
    /// the regression policy discarded it.
    RolledBack,
    /// `merge_requires_improvement` refused it: nothing regressed, but nothing improved either. Not
    /// a rollback — it neither counts for `rollbacks` nor extends or breaks the streak.
    NoImprovement,
    /// nothing was decided (no changes, a veto, a conflict).
    Nothing,
}

/// `oscillating`'s default window: how many earlier sessions a repeat may reach back to.
pub const DEFAULT_OSCILLATION_WINDOW: usize = 4;

/// How many signatures are kept. Far above any sane window; it only bounds the persisted file.
const HISTORY_CAP: usize = 64;

/// The gate's persisted state — everything the terms read. `agg/private/gate.json`, and a copy on
/// every driver `Gate` call record so a resume restores it as of the last kept gate.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GateState {
    /// the ratchet references: each ratcheted judge's value at the last kept gate (or its seed).
    #[serde(default)]
    pub refs: BTreeMap<String, f64>,
    /// sessions the regression policy rolled back, this run.
    #[serde(default)]
    pub rollbacks: u32,
    /// rollbacks since the last kept gate.
    #[serde(default)]
    pub consecutive_rollbacks: u32,
    /// sessions `merge_requires_improvement` refused this run — never counted as rollbacks.
    #[serde(default)]
    pub no_improvements: u32,
    /// no_improvement gates since the last kept gate. A rollback neither extends nor breaks it.
    #[serde(default)]
    pub consecutive_no_improvements: u32,
    /// one signature per judged gate, oldest first: the sorted names of the judges that were
    /// failing (see [`signature`]).
    #[serde(default)]
    pub signatures: Vec<Vec<String>>,
    /// the ratchet regressions of the MOST RECENT judged gate (backs `ratchet_regressed`).
    #[serde(default)]
    pub last_ratchet: Vec<RatchetRegression>,
}

impl GateState {
    /// The reference a ratcheted judge is currently held to.
    pub fn reference(&self, judge: &str) -> Option<f64> {
        self.refs.get(judge).copied()
    }

    /// Did the most recent judged gate record a ratchet regression?
    pub fn ratchet_regressed(&self) -> bool {
        !self.last_ratchet.is_empty()
    }

    /// A→B→A: the latest signature is non-empty and equals one of the `window` signatures before it,
    /// with at least one DIFFERENT signature in between. An all-green signature never counts —
    /// returning to a good state is not oscillation — and fewer than three judged gates never do.
    pub fn oscillating(&self, window: usize) -> bool {
        let Some((cur, earlier)) = self.signatures.split_last() else { return false };
        if cur.is_empty() {
            return false;
        }
        let recent = &earlier[earlier.len().saturating_sub(window)..];
        recent
            .iter()
            .enumerate()
            .any(|(i, s)| s == cur && recent[i + 1..].iter().any(|t| t != cur))
    }
}

/// The ratchet configuration plus the state it drives — owned by the engine, so the terms read the
/// loop's own memory.
#[derive(Debug, Clone)]
pub struct Gate {
    pub specs: BTreeMap<String, Ratchet>,
    pub window: usize,
    pub state: GateState,
}

impl Default for Gate {
    fn default() -> Self {
        Gate::new(BTreeMap::new(), DEFAULT_OSCILLATION_WINDOW)
    }
}

impl Gate {
    /// A fresh gate; every seeded `baseline:` is the reference from the start.
    pub fn new(specs: BTreeMap<String, Ratchet>, window: usize) -> Gate {
        let refs = specs.iter().filter_map(|(n, s)| s.baseline.map(|b| (n.clone(), b))).collect();
        Gate { specs, window, state: GateState { refs, ..GateState::default() } }
    }

    pub fn is_ratcheted(&self, judge: &str) -> bool {
        self.specs.contains_key(judge)
    }

    /// `oscillating` at the configured window.
    pub fn oscillating(&self) -> bool {
        self.state.oscillating(self.window)
    }

    /// Take the references from the pre-session baseline pass. Returns the warnings to print.
    ///
    /// A seed wins over the measurement; a gap between them beyond tolerance is reported as drift.
    /// Refused: a ratcheted judge that emits no number (no ratchet can hold it), and one that
    /// errors with no seed to fall back on (there would be nothing to compare session 1 against).
    /// A judge missing from `baseline` is skipped — the driver path has no baseline pass.
    pub fn seed(&mut self, baseline: &[(String, Verdict)]) -> Result<Vec<String>> {
        let mut warnings = Vec::new();
        for (name, spec) in &self.specs {
            let Some((_, v)) = baseline.iter().find(|(n, _)| n == name) else { continue };
            if let Some(w) = Self::seed_one(&mut self.state.refs, name, spec, v)? {
                warnings.push(w);
            }
        }
        Ok(warnings)
    }

    fn seed_one(refs: &mut BTreeMap<String, f64>, name: &str, spec: &Ratchet, v: &Verdict) -> Result<Option<String>> {
        if v.error.is_none() && finite(v).is_none() {
            bail!("ratcheted judge `{name}` emitted no `value` — a ratchet compares numbers, so this judge cannot be ratcheted");
        }
        match (spec.baseline, finite(v), &v.error) {
            (Some(seed), Some(now), _) if (now - seed).abs() > spec.allowance(seed) => Ok(Some(format!(
                "ratchet `{name}`: the seeded baseline {seed} and the measured {now} differ by more than the tolerance — using the seed (measurement drift?)"
            ))),
            (Some(_), _, Some(e)) => Ok(Some(format!("ratchet `{name}`: the baseline pass errored ({e}) — using the seed"))),
            (Some(_), _, _) => Ok(None),
            (None, _, Some(e)) => {
                bail!("ratcheted judge `{name}` errored on the baseline pass and has no `baseline:` seed to fall back on: {e}")
            }
            (None, now, None) => {
                refs.insert(name.to_string(), now.expect("checked above"));
                Ok(None)
            }
        }
    }

    /// The driver's baseline: a ratcheted judge asked BEFORE the first step sets the reference
    /// (unless a seed already did). Returns a warning to print, if any.
    pub fn observe_pre_step(&mut self, name: &str, v: &Verdict) -> Option<String> {
        let spec = self.specs.get(name)?;
        if spec.baseline.is_none() && self.state.refs.contains_key(name) {
            return None;
        }
        match Self::seed_one(&mut self.state.refs, name, spec, v) {
            Ok(w) => w,
            Err(e) => Some(format!("{e:#} — every gate will treat it as a ratchet regression")),
        }
    }

    /// The ratchet regressions among `fresh`. A ratcheted judge that errored or has no number is a
    /// regression whatever its reference; one with no reference yet cannot regress.
    pub fn check(&self, fresh: &[(String, Verdict)]) -> Vec<RatchetRegression> {
        fresh
            .iter()
            .filter_map(|(name, v)| {
                let spec = self.specs.get(name)?;
                let from = self.state.reference(name);
                let reg = |to| Some(RatchetRegression { judge: name.clone(), from, to });
                match (finite(v).filter(|_| v.error.is_none()), from) {
                    (None, _) => reg(None),
                    (Some(now), Some(r)) if spec.regressed(r, now) => reg(Some(now)),
                    _ => None,
                }
            })
            .collect()
    }

    /// The ratcheted judges among `fresh` that IMPROVED on their reference beyond tolerance. One with
    /// no reference yet, an error or no number cannot show an improvement.
    pub fn improved(&self, fresh: &[(String, Verdict)]) -> Vec<String> {
        fresh
            .iter()
            .filter(|(name, v)| {
                let (Some(spec), Some(r)) = (self.specs.get(name), self.state.reference(name)) else { return false };
                finite(v).filter(|_| v.error.is_none()).is_some_and(|now| spec.improved(r, now))
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Fold one gate into the state: counters, the signature history, and — on a KEEP — the new
    /// references. `fresh` empty = no judge ran, so there is no signature to record.
    pub fn record(
        &mut self,
        decision: GateDecision,
        fresh: &[(String, Verdict)],
        ratchet: &[RatchetRegression],
        signature: Vec<String>,
    ) {
        let st = &mut self.state;
        match decision {
            GateDecision::Kept => {
                st.consecutive_rollbacks = 0;
                st.consecutive_no_improvements = 0;
                for (name, v) in fresh.iter().filter(|(n, _)| self.specs.contains_key(n)) {
                    if let Some(now) = finite(v).filter(|_| v.error.is_none()) {
                        st.refs.insert(name.clone(), now);
                    }
                }
            }
            GateDecision::RolledBack => {
                st.rollbacks += 1;
                st.consecutive_rollbacks += 1;
            }
            GateDecision::NoImprovement => {
                st.no_improvements += 1;
                st.consecutive_no_improvements += 1;
            }
            GateDecision::Nothing => {}
        }
        if !fresh.is_empty() {
            st.last_ratchet = ratchet.to_vec();
            st.signatures.push(signature);
            let excess = st.signatures.len().saturating_sub(HISTORY_CAP);
            st.signatures.drain(..excess);
        }
    }
}

/// A finite `value`, or `None` — NaN and infinities are not measurements.
fn finite(v: &Verdict) -> Option<f64> {
    v.value.filter(|n| n.is_finite())
}

/// A gate's judge-state signature: the sorted names of the in-scope judges that are failing (not
/// met, or errored) plus every judge that regressed its ratchet.
pub fn signature(
    fresh: &[(String, Verdict)],
    in_scope: impl Fn(&str) -> bool,
    ratchet: &[RatchetRegression],
) -> Vec<String> {
    let mut out: Vec<String> = fresh
        .iter()
        .filter(|(n, v)| in_scope(n) && (!v.met || v.error.is_some()))
        .map(|(n, _)| n.clone())
        .chain(ratchet.iter().map(|r| r.judge.clone()))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// One line per regression, for the operator's terminal.
pub fn describe(regs: &[RatchetRegression]) -> Vec<String> {
    let num = |n: Option<f64>| n.map(|v| v.to_string()).unwrap_or_else(|| "no value".into());
    regs.iter().map(|r| format!("ratchet `{}`: {} → {}", r.judge, num(r.from), num(r.to))).collect()
}

/// Persist the state to `agg/private/gate.json`. A hard error, like `verdicts::append`: a resume
/// reads it back, and a silently stale file would hand the run a clean rollback count.
pub fn save(dir: &Path, state: &GateState) -> Result<()> {
    let path = crate::paths::gate_json(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(state).context("serializing the gate state")?;
    crate::util::write_atomic(&path, &text).with_context(|| format!("writing {}", path.display()))
}

/// The persisted state, if there is a readable one.
pub fn load(dir: &Path) -> Option<GateState> {
    let text = std::fs::read_to_string(crate::paths::gate_json(dir)).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests;
