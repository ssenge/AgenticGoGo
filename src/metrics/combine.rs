//! Several tools, one metric: which value the judge reports and whether it is met — pure.
//!
//! The values are ordered from the one that passes the threshold most easily to the one that passes
//! it least (by the metric's direction; for a `neutral` metric, by the threshold itself), and:
//! - `strictest` reports the LAST — the value hardest to pass;
//! - `all` reports the same value (every tool passes iff the hardest one does), and is `mixed` when
//!   the tools moved in DIFFERENT directions since the incumbent — no improvement for a ratchet;
//! - `majority` reports the k-th, k = ⌊n/2⌋+1 — more than half the tools pass iff that one does.

use super::report::{Better, ToolValue};
use super::spec::Combine;

/// What a metric judge concludes from the selected values.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub value: f64,
    pub met: bool,
    /// the metric's direction, as the tools state it.
    pub better: Better,
    /// the tools disagree on whether the metric got better or worse since the incumbent.
    pub mixed: bool,
    /// how the value came about, for the rationale (`lizard`, `strictest of a 3, b 4`).
    pub how: String,
}

/// The threshold: met when at most `max` and at least `min`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Threshold {
    pub max: Option<f64>,
    pub min: Option<f64>,
}

impl Threshold {
    pub fn passes(&self, v: f64) -> bool {
        self.max.is_none_or(|m| v <= m) && self.min.is_none_or(|m| v >= m)
    }

    /// `≤ 10`, `≥ 80`, `within [1, 5]`.
    pub fn describe(&self) -> String {
        match (self.max, self.min) {
            (Some(hi), Some(lo)) => format!("within [{lo}, {hi}]"),
            (Some(hi), None) => format!("≤ {hi}"),
            (None, Some(lo)) => format!("≥ {lo}"),
            (None, None) => "unbounded".into(),
        }
    }

    /// For a `neutral` metric the threshold says which way is better: under a `max`, lower.
    fn implied(&self) -> Better {
        if self.max.is_some() {
            Better::Lower
        } else {
            Better::Higher
        }
    }
}

/// Decide over `values` (one per tool, never empty). `incumbent` = the same selection on the
/// incumbent's report, for `all`'s disagreement check.
pub fn decide(
    id: &str,
    values: &[ToolValue],
    combine: Option<Combine>,
    t: Threshold,
    incumbent: Option<&[ToolValue]>,
) -> Result<Decision, String> {
    let better = direction(id, values)?;
    if values.len() == 1 {
        let v = &values[0];
        return Ok(Decision { value: v.value, met: t.passes(v.value), better, mixed: false, how: v.tool.clone() });
    }
    let Some(how) = combine else {
        let tools: Vec<&str> = values.iter().map(|v| v.tool.as_str()).collect();
        return Err(format!(
            "several tools report `{id}` ({}) — name one with `tool:` or merge them with `combine: strictest | all | majority`",
            tools.join(", ")
        ));
    };
    let order = if better == Better::Neutral { t.implied() } else { better };
    let mut sorted: Vec<&ToolValue> = values.iter().collect();
    sorted.sort_by(|a, b| {
        let o = a.value.total_cmp(&b.value);
        if order == Better::Higher { o.reverse() } else { o }
    });
    let listed = sorted.iter().map(|v| format!("{} {}", v.tool, v.value)).collect::<Vec<_>>().join(", ");
    let (value, met, name) = match how {
        Combine::Strictest | Combine::All => {
            let worst = sorted[sorted.len() - 1].value;
            let met = values.iter().all(|v| t.passes(v.value));
            (worst, met, if how == Combine::All { "all of" } else { "strictest of" })
        }
        Combine::Majority => {
            let k = values.len() / 2 + 1;
            let passing = values.iter().filter(|v| t.passes(v.value)).count();
            (sorted[k - 1].value, passing >= k, "majority of")
        }
    };
    let mixed = how == Combine::All && incumbent.is_some_and(|inc| disagree(values, inc, better));
    Ok(Decision { value, met, better, mixed, how: format!("{name} {listed}") })
}

/// The one direction the tools state for the metric; tools that contradict each other are an error.
fn direction(id: &str, values: &[ToolValue]) -> Result<Better, String> {
    let first = values[0].better;
    match values.iter().find(|v| v.better != first) {
        None => Ok(first),
        Some(v) => Err(format!(
            "the tools disagree on which way is better for `{id}` ({} says {}, {} says {})",
            values[0].tool,
            first.as_str(),
            v.tool,
            v.better.as_str()
        )),
    }
}

/// Did some tool's value get better and another's worse since the incumbent? A neutral metric has
/// no better, so it never disagrees.
fn disagree(now: &[ToolValue], incumbent: &[ToolValue], better: Better) -> bool {
    let moves: Vec<f64> = now
        .iter()
        .filter_map(|v| incumbent.iter().find(|i| i.tool == v.tool).map(|i| v.value - i.value))
        .map(|d| if better == Better::Higher { d } else { -d })
        .collect();
    better != Better::Neutral && moves.iter().any(|m| *m > 0.0) && moves.iter().any(|m| *m < 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv(tool: &str, value: f64, better: Better) -> ToolValue {
        ToolValue { tool: tool.into(), value, better }
    }

    const MAX10: Threshold = Threshold { max: Some(10.0), min: None };

    #[test]
    fn one_tool_is_its_own_value() {
        let d = decide("m.x", &[tv("lizard", 6.0, Better::Lower)], None, MAX10, None).unwrap();
        assert_eq!((d.value, d.met, d.how.as_str(), d.mixed), (6.0, true, "lizard", false));
        let d = decide("m.x", &[tv("lizard", 11.0, Better::Lower)], None, MAX10, None).unwrap();
        assert!(!d.met);
    }

    #[test]
    fn several_tools_without_combine_is_an_error_naming_them() {
        let e = decide("m.x", &[tv("a", 1.0, Better::Lower), tv("b", 2.0, Better::Lower)], None, MAX10, None).unwrap_err();
        assert!(e.contains("a, b") && e.contains("combine:"), "{e}");
    }

    #[test]
    fn strictest_and_all_report_the_hardest_value_majority_the_kth() {
        let vals = [tv("a", 4.0, Better::Lower), tv("b", 12.0, Better::Lower), tv("c", 8.0, Better::Lower)];
        let s = decide("m.x", &vals, Some(Combine::Strictest), MAX10, None).unwrap();
        assert_eq!((s.value, s.met), (12.0, false));
        assert_eq!(s.how, "strictest of a 4, c 8, b 12");
        let a = decide("m.x", &vals, Some(Combine::All), MAX10, None).unwrap();
        assert_eq!((a.value, a.met), (12.0, false));
        let m = decide("m.x", &vals, Some(Combine::Majority), MAX10, None).unwrap();
        assert_eq!((m.value, m.met), (8.0, true), "two of three within 10");
        // higher is better: the hardest is the LOWEST.
        let cov = [tv("a", 70.0, Better::Higher), tv("b", 90.0, Better::Higher)];
        let min80 = Threshold { max: None, min: Some(80.0) };
        let s = decide("c.p", &cov, Some(Combine::Strictest), min80, None).unwrap();
        assert_eq!((s.value, s.met), (70.0, false));
        let m = decide("c.p", &cov, Some(Combine::Majority), min80, None).unwrap();
        assert_eq!((m.value, m.met), (70.0, false), "of two tools a majority is both");
    }

    #[test]
    fn a_neutral_metric_orders_by_its_threshold() {
        let vals = [tv("a", 3.0, Better::Neutral), tv("b", 9.0, Better::Neutral)];
        assert_eq!(decide("s.f", &vals, Some(Combine::Strictest), Threshold { max: Some(5.0), min: None }, None).unwrap().value, 9.0);
        assert_eq!(decide("s.f", &vals, Some(Combine::Strictest), Threshold { max: None, min: Some(5.0) }, None).unwrap().value, 3.0);
    }

    /// `all`: tools moving in DIFFERENT directions since the incumbent is `mixed` — no improvement.
    #[test]
    fn all_marks_tools_that_disagree_on_the_direction_of_change() {
        let inc = [tv("a", 10.0, Better::Lower), tv("b", 6.0, Better::Lower)];
        let split = [tv("a", 8.0, Better::Lower), tv("b", 7.0, Better::Lower)];
        assert!(decide("m.x", &split, Some(Combine::All), MAX10, Some(&inc)).unwrap().mixed);
        let both_better = [tv("a", 8.0, Better::Lower), tv("b", 5.0, Better::Lower)];
        assert!(!decide("m.x", &both_better, Some(Combine::All), MAX10, Some(&inc)).unwrap().mixed);
        assert!(!decide("m.x", &split, Some(Combine::Strictest), MAX10, Some(&inc)).unwrap().mixed, "only `all` marks it");
        assert!(!decide("m.x", &split, Some(Combine::All), MAX10, None).unwrap().mixed, "no incumbent, nothing to compare");
        let neutral = [tv("a", 8.0, Better::Neutral), tv("b", 7.0, Better::Neutral)];
        let neutral_inc = [tv("a", 10.0, Better::Neutral), tv("b", 6.0, Better::Neutral)];
        assert!(!decide("m.x", &neutral, Some(Combine::All), MAX10, Some(&neutral_inc)).unwrap().mixed);
    }

    #[test]
    fn tools_contradicting_the_direction_are_an_error_and_thresholds_describe_themselves() {
        let e = decide("m.x", &[tv("a", 1.0, Better::Lower), tv("b", 1.0, Better::Higher)], Some(Combine::All), MAX10, None);
        assert!(e.unwrap_err().contains("disagree"));
        assert_eq!(MAX10.describe(), "≤ 10");
        assert_eq!(Threshold { max: None, min: Some(80.0) }.describe(), "≥ 80");
        assert_eq!(Threshold { max: Some(5.0), min: Some(1.0) }.describe(), "within [1, 5]");
        assert_eq!(Threshold { max: None, min: None }.describe(), "unbounded");
        assert!(Threshold { max: Some(5.0), min: Some(1.0) }.passes(3.0));
    }
}
