//! The YAML walk (RUST_API §11.2) — the whole of the YAML path's flow, in ~30 lines.
//!
//! ```text
//! for entry in sequence.steps:         # laps forever, until done_if or a ceiling
//!     for _ in 0 .. entry.reps():      # times / until+max / once
//!         dispatch(entry.step)
//!         if entry.until and eval(entry.until): break
//! ```
//!
//! It replaces `core::sequence`'s `Cursor` over a hand-parsed statement list, and it dispatches
//! through the SAME primitive a Rust driver's `agg.step()` calls — one execution primitive, two
//! drivers over it. There is no `if:` (owner simplification, 2026-08-04, §14.14), so unlike
//! `Cursor` a lap ALWAYS dispatches: the no-dispatch-lap refusal is unrepresentable here rather
//! than guarded.
//!
//! The walk is written as a cursor rather than as literal nested `for`s because the caller is the
//! handler pipeline: `PickStep` asks for ONE step name per session and everything between two
//! dispatches (the worker, the judges, the gate) happens in between. `until` is therefore evaluated
//! at the top of the next lap — after the dispatch it is judging, which is the same order the
//! pseudo-code above reads in.

use crate::core::config::SeqStep;
use anyhow::{bail, Result};

/// A cursor over `sequence.steps` yielding ONE step name per call, wrapping forever (§5.5).
/// The `until` expression is evaluated by a caller-supplied closure, so the walk stays free of
/// judge state.
pub struct Walk {
    entries: Vec<SeqStep>,
    idx: usize,
    /// dispatches of `entries[idx]` so far in this visit.
    reps: u32,
}

impl Walk {
    pub fn new(entries: Vec<SeqStep>) -> Self {
        Walk { entries, idx: 0, reps: 0 }
    }

    /// The next step to dispatch. `eval` decides an `until` expression.
    ///
    /// Errors only on an EMPTY entry list, which never reaches here on either path — a driver
    /// project's steps arrive through `ctx.next_step` (`PickStep` takes it first) and `agg run`
    /// needs a list. It bails rather than indexing into nothing.
    pub fn next_step(&mut self, eval: &mut impl FnMut(&str) -> Result<bool>) -> Result<String> {
        if self.entries.is_empty() {
            bail!("the sequence is empty — `agg run` needs at least one step in `sequence.steps`");
        }
        loop {
            if self.idx >= self.entries.len() {
                self.idx = 0; // wrap
            }
            let e = &self.entries[self.idx];
            // `until` is checked only AFTER a dispatch — a condition that is already true at the
            // top of a lap still buys one session, exactly as `repeat … until` reads.
            let converged = match (&e.until, self.reps) {
                (Some(c), r) if r > 0 => eval(c)?,
                _ => false,
            };
            // `max` bounds `until`; `times` is a fixed count; a plain entry runs once. `assemble`
            // refuses `until` without `max` and any count below 1, so `cap >= 1` always and the
            // loop below can advance at most `entries.len()` times before it dispatches.
            let cap = e.times.or(e.max).unwrap_or(1);
            if converged || self.reps >= cap {
                self.idx += 1;
                self.reps = 0;
                continue;
            }
            self.reps += 1;
            return Ok(e.step.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(step: &str, times: Option<u32>, until: Option<&str>, max: Option<u32>) -> SeqStep {
        SeqStep { step: step.into(), times, until: until.map(String::from), max }
    }

    /// The dispatch order, with no condition to evaluate.
    fn plain(w: &mut Walk, n: usize) -> Vec<String> {
        (0..n).map(|_| w.next_step(&mut |_| Ok(false)).unwrap()).collect()
    }

    /// TODAY'S ORDER, REPRODUCED: a bare list dispatches each entry once per lap and wraps forever
    /// — what `Cursor` did for `Statement::Step`.
    #[test]
    fn a_plain_list_laps_forever_in_order() {
        let mut w = Walk::new(vec![entry("plan", None, None, None), entry("build", None, None, None)]);
        assert_eq!(plain(&mut w, 5), vec!["plan", "build", "plan", "build", "plan"]);
    }

    /// `times: n` is the successor of `worker x4`, and it dispatches the same way: n in a row, then
    /// on to the next entry, then round again.
    #[test]
    fn times_dispatches_the_step_n_times_then_moves_on() {
        let mut w = Walk::new(vec![entry("w", Some(3), None, None), entry("r", None, None, None)]);
        assert_eq!(plain(&mut w, 5), vec!["w", "w", "w", "r", "w"]);
    }

    /// `until` + `max`: repeat until the condition holds …
    #[test]
    fn until_stops_repeating_as_soon_as_the_condition_holds() {
        let mut w = Walk::new(vec![entry("fix", None, Some("green"), Some(8)), entry("ship", None, None, None)]);
        // one dispatch happens before the condition is ever consulted (`repeat … until`), so a
        // condition true from the start still buys exactly one session.
        assert_eq!(w.next_step(&mut |_| Ok(true)).unwrap(), "fix");
        assert_eq!(w.next_step(&mut |_| Ok(true)).unwrap(), "ship");
    }

    /// … but never more than `max` times, however stubborn the condition.
    #[test]
    fn max_bounds_an_until_that_never_holds() {
        let mut w = Walk::new(vec![entry("fix", None, Some("green"), Some(3)), entry("ship", None, None, None)]);
        assert_eq!(plain(&mut w, 5), vec!["fix", "fix", "fix", "ship", "fix"]);
    }

    /// The condition is re-evaluated between dispatches, not sampled once.
    #[test]
    fn until_is_evaluated_after_every_dispatch() {
        let mut w = Walk::new(vec![entry("fix", None, Some("green"), Some(8)), entry("ship", None, None, None)]);
        let mut answers = [false, true].into_iter();
        let got: Vec<String> = (0..3)
            .map(|_| w.next_step(&mut |_| Ok(answers.next().unwrap_or(false))).unwrap())
            .collect();
        assert_eq!(got, vec!["fix", "fix", "ship"]);
    }

    /// An `until` that errors (a broken expression) surfaces to the caller rather than being read
    /// as "not converged" — the loop turns it into an abort with the message attached.
    #[test]
    fn an_eval_error_propagates() {
        let mut w = Walk::new(vec![entry("fix", None, Some("boom"), Some(2))]);
        w.next_step(&mut |_| Ok(false)).unwrap();
        let err = w.next_step(&mut |_| anyhow::bail!("bad expression")).unwrap_err().to_string();
        assert!(err.contains("bad expression"), "got: {err}");
    }

    /// A driver project never reaches the walk; if it somehow does, it must bail, not panic.
    #[test]
    fn an_empty_walk_bails_instead_of_indexing_nothing() {
        let err = Walk::new(vec![]).next_step(&mut |_| Ok(false)).unwrap_err().to_string();
        assert!(err.contains("at least one step"), "got: {err}");
    }
}
