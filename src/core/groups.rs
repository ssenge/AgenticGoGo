//! Judge GROUPS (`sequence.groups`) and the gate's PROGRESS rule over them.
//!
//! A group is a NAME for a list of judges — `quality: [sonar, codescene, lizard]` — so the
//! aggregates can range over it: `count_met(quality) >= 2` is "two of three". The group carries no
//! threshold; the condition owns it, exactly as it owns every other threshold.
//!
//! # A group is its members
//!
//! Wherever a condition names a group, it names the group's members: a group in `done_if` puts them
//! in the DoD-set, a group in `abort_if` / `notify_if` / an `until:` puts them in the run-set only.
//! Every member of every group joins the run-set, named or not.
//!
//! # …except at the gate
//!
//! The gate's boolean rule ("a judge that was met is now unmet") is per JUDGE. For a judge that is
//! in the DoD-set ONLY because a `done_if` group holds it, the gate judges the GROUP instead: the
//! group regressed when more of its members went met→unmet than unmet→met. That is what makes k-of-n
//! worth having — tools disagree, so one of three flipping while another catches up is a swap, not a
//! regression — while a group that lost ground net is still rolled back. A member that is ALSO named
//! on its own (bare in `done_if`, or in `invariants`) is held to the per-judge rule as well.
//!
//! The same counting decides "improved" for `sequence.merge_requires_improvement`: an individually
//! judged goal went unmet→met, or a group gained ground net. [`assess`] computes both, for both
//! front ends — the YAML gate and the driver's `gate()` differ only in the scope they hand it.

use crate::core::model::Verdict;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The terms and keywords a group may not be called: the grammar would read them as something else
/// (or, for `invariants`, as the other subset).
const RESERVED: &[&str] = &[
    "invariants", "all_goals", "count_met", "count_regressed", "total", "met_fraction", "any_regressed",
    "and", "or", "not", "AND", "OR", "NOT",
];

/// The configured groups, plus what the gate needs to know about them.
#[derive(Debug, Clone, Default)]
pub struct Groups {
    /// group name → member judge names, as configured.
    pub map: BTreeMap<String, Vec<String>>,
    /// the groups `done_if` names — the ones the gate judges as a unit.
    pub gated: BTreeSet<String>,
    /// the judges `done_if` names bare, plus every invariant — held to the per-judge rule even when a
    /// gated group also holds them.
    pub direct: BTreeSet<String>,
}

/// No groups at all — what a context with none configured reads.
static NONE: Groups = Groups { map: BTreeMap::new(), gated: BTreeSet::new(), direct: BTreeSet::new() };

impl Groups {
    /// The empty set of groups, for contexts that have none.
    pub fn none() -> &'static Groups {
        &NONE
    }

    /// Just the map — the shape a condition needs to resolve `count_met(<group>)`.
    pub fn from_map(map: BTreeMap<String, Vec<String>>) -> Groups {
        Groups { map, ..Groups::default() }
    }

    /// A group's members, if `name` is a group.
    pub fn members(&self, name: &str) -> Option<&[String]> {
        self.map.get(name).map(Vec::as_slice)
    }

    /// The configured group names, for "defined groups: …" hints.
    pub fn names(&self) -> Vec<&str> {
        self.map.keys().map(String::as_str).collect()
    }

    /// Is `judge` judged by the gate ONLY through a gated group (never on its own)?
    pub fn group_only(&self, judge: &str) -> bool {
        !self.direct.contains(judge) && self.gated.iter().any(|g| self.map.get(g).is_some_and(|m| m.iter().any(|n| n == judge)))
    }

    /// The member lists of the gated groups — the group half of the gate's scope.
    pub fn gated_members(&self) -> Vec<&[String]> {
        self.gated.iter().filter_map(|g| self.members(g)).collect()
    }
}

/// The refusals for ONE group: a name the grammar cannot read as a group, no members, a member listed
/// twice. Each is `(config path, message)`; member resolution is the caller's (it owns the files).
pub fn problems(name: &str, members: &[String]) -> Vec<(String, String)> {
    let at = format!("sequence.groups.{name}");
    let mut out = Vec::new();
    let ident = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ident {
        out.push((at.clone(), format!("group name `{name}` is not an identifier — use letters, digits and `_`, starting with a letter or `_`")));
    } else if RESERVED.contains(&name) {
        out.push((at.clone(), format!("group name `{name}` is reserved by the condition grammar — pick another name")));
    }
    if members.is_empty() {
        out.push((at.clone(), format!("group `{name}` is empty — an aggregate over no judges can never mean anything")));
    }
    for (i, m) in members.iter().enumerate() {
        if members[..i].contains(m) {
            out.push((format!("{at}[{i}]"), format!("group `{name}` lists `{m}` twice — it would be counted twice")));
        }
    }
    out
}

/// What one gate made of its span: did it lose ground, did it gain any?
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// the boolean regression rule fired (per judge, or net over a group).
    pub regressed: bool,
    /// something got better: a judge went unmet→met, or a group gained ground net.
    pub improved: bool,
}

/// The boolean half of the gate over `fresh`, against `landed` (each judge's last LANDED, graded
/// `met`). `individual` says which judges are held one by one; `groups` are judged net, over the
/// members that are NOT individual. An errored verdict counts for neither side — "could not grade"
/// is not "not met", exactly as in the per-judge rule.
pub fn assess(
    fresh: &[(String, Verdict)],
    landed: &HashMap<String, bool>,
    individual: impl Fn(&str) -> bool,
    groups: &[&[String]],
) -> Progress {
    let was = |n: &str| landed.get(n).copied().unwrap_or(false);
    let graded = |n: &str| fresh.iter().find(|(id, v)| id == n && v.error.is_none()).map(|(_, v)| v.met);
    let mut p = Progress::default();
    for (n, v) in fresh.iter().filter(|(n, v)| v.error.is_none() && individual(n)) {
        p.regressed |= !v.met && was(n);
        p.improved |= v.met && !was(n);
    }
    for members in groups {
        let (mut gain, mut loss) = (0usize, 0usize);
        for m in members.iter().filter(|m| !individual(m)) {
            match (graded(m), was(m)) {
                (Some(true), false) => gain += 1,
                (Some(false), true) => loss += 1,
                _ => {}
            }
        }
        p.regressed |= loss > gain;
        p.improved |= gain > loss;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(met: bool) -> Verdict {
        Verdict::binary(met)
    }

    fn landed(pairs: &[(&str, bool)]) -> HashMap<String, bool> {
        pairs.iter().map(|(n, m)| (n.to_string(), *m)).collect()
    }

    fn fresh(pairs: &[(&str, bool)]) -> Vec<(String, Verdict)> {
        pairs.iter().map(|(n, m)| (n.to_string(), v(*m))).collect()
    }

    fn names(ns: &[&str]) -> Vec<String> {
        ns.iter().map(|s| s.to_string()).collect()
    }

    /// The per-judge rule: met→unmet regresses, unmet→met (or never landed → met) improves.
    #[test]
    fn an_individual_judge_regresses_and_improves_one_by_one() {
        let l = landed(&[("a", true), ("b", false)]);
        let p = assess(&fresh(&[("a", false), ("b", false)]), &l, |_| true, &[]);
        assert_eq!(p, Progress { regressed: true, improved: false });
        let p = assess(&fresh(&[("a", true), ("b", true)]), &l, |_| true, &[]);
        assert_eq!(p, Progress { regressed: false, improved: true });
        let p = assess(&fresh(&[("new", true)]), &l, |_| true, &[]);
        assert!(p.improved, "a judge with no landed row was never met, so met now is an improvement");
        let p = assess(&fresh(&[("a", true)]), &l, |_| true, &[]);
        assert_eq!(p, Progress::default(), "met → met is neither");
    }

    /// An errored verdict is neither a loss nor a gain, on either rule.
    #[test]
    fn an_errored_verdict_counts_for_nothing() {
        let l = landed(&[("a", true), ("q1", true)]);
        let errs = vec![("a".to_string(), Verdict::failed("boom")), ("q1".to_string(), Verdict::failed("boom"))];
        let q = names(&["q1"]);
        assert_eq!(assess(&errs, &l, |n| n == "a", &[&q]), Progress::default());
    }

    /// A group is judged NET: a swap is neither, a net loss regresses, a net gain improves.
    #[test]
    fn a_group_is_judged_net_over_its_members() {
        let q = names(&["a", "b", "c"]);
        let l = landed(&[("a", true), ("b", true), ("c", false)]);
        let none = |_: &str| false;
        // swap: a lost, c gained.
        let p = assess(&fresh(&[("a", false), ("b", true), ("c", true)]), &l, none, &[&q]);
        assert_eq!(p, Progress::default(), "a swap is not a regression and not an improvement");
        // net loss.
        let p = assess(&fresh(&[("a", false), ("b", true), ("c", false)]), &l, none, &[&q]);
        assert_eq!(p, Progress { regressed: true, improved: false });
        // net gain.
        let p = assess(&fresh(&[("a", true), ("b", true), ("c", true)]), &l, none, &[&q]);
        assert_eq!(p, Progress { regressed: false, improved: true });
        // a member that was not asked counts for nothing.
        let p = assess(&fresh(&[("c", true)]), &l, none, &[&q]);
        assert!(p.improved && !p.regressed);
    }

    /// A member that is ALSO individual is held one by one and left out of the group's count.
    #[test]
    fn an_individual_member_is_held_on_its_own() {
        let q = names(&["a", "b"]);
        let l = landed(&[("a", true), ("b", false)]);
        let p = assess(&fresh(&[("a", false), ("b", true)]), &l, |n| n == "a", &[&q]);
        assert_eq!(p, Progress { regressed: true, improved: true }, "a regressed on its own; b is the group's gain");
    }

    /// The refusals: a bad name, a reserved name, no members, a duplicate.
    #[test]
    fn a_group_that_cannot_mean_anything_is_refused() {
        assert!(problems("quality", &names(&["a", "b"])).is_empty());
        let p = problems("9x", &names(&["a"]));
        assert!(p[0].1.contains("not an identifier"), "{p:?}");
        let p = problems("invariants", &names(&["a"]));
        assert!(p[0].1.contains("reserved"), "{p:?}");
        let p = problems("q", &[]);
        assert_eq!(p[0].0, "sequence.groups.q");
        assert!(p[0].1.contains("empty"));
        let p = problems("q", &names(&["a", "b", "a"]));
        assert_eq!(p, vec![("sequence.groups.q[2]".into(), "group `q` lists `a` twice — it would be counted twice".into())]);
    }

    /// `group_only` — the gate's question: is this judge held only through a gated group?
    #[test]
    fn group_only_is_a_gated_member_not_named_directly() {
        let mut g = Groups::from_map(BTreeMap::from([("q".to_string(), names(&["a", "b"])), ("r".to_string(), names(&["c"]))]));
        g.gated.insert("q".into());
        g.direct.insert("b".into());
        assert!(g.group_only("a"));
        assert!(!g.group_only("b"), "named on its own too");
        assert!(!g.group_only("c"), "its group is not in done_if");
        assert_eq!(g.gated_members(), vec![&names(&["a", "b"])[..]]);
        assert_eq!(g.names(), vec!["q", "r"]);
        assert!(Groups::none().members("q").is_none());
    }
}
