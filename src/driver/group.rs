//! Judge GROUPS on the driver path — `sequence.groups` and `count_met(<group>)` spelled in Rust.
//!
//! A [`Group`] names a list of judges; [`crate::driver::Agg::group`] asks every member (each through
//! the same lazy, memoized cache as `agg.judge()`) and returns a [`GroupVerdict`] to count over. The
//! group carries no threshold: "two of three" is `agg.group(&quality).at_least(2)`, written where
//! the driver branches, exactly as the YAML path writes `count_met(quality) >= 2` in the condition.
//!
//! At [`crate::driver::Agg::gate`], a judge asked ONLY through a group is judged with its group,
//! net — the rule `crate::core::groups` documents — while one the driver also asked on its own is
//! held one by one.

use crate::core::model::{Judge, Verdict};

/// A named list of judges: `Group::new("quality", [sonar, codescene, lizard])`.
#[derive(Clone)]
pub struct Group {
    pub(crate) name: String,
    pub(crate) members: Vec<Judge>,
}

impl Group {
    /// A group of `members`. The name is what the gate and the logs call it.
    pub fn new(name: impl Into<String>, members: impl IntoIterator<Item = Judge>) -> Group {
        Group { name: name.into(), members: members.into_iter().collect() }
    }

    /// The group's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The member judges' names, in order.
    pub fn member_names(&self) -> Vec<String> {
        self.members.iter().map(|j| j.name.clone()).collect()
    }
}

/// Every member's verdict for this step, in member order — what the aggregates count over.
#[derive(Debug, Clone)]
pub struct GroupVerdict {
    pub name: String,
    pub verdicts: Vec<(String, Verdict)>,
}

impl GroupVerdict {
    /// How many members are met — `count_met(<group>)`.
    pub fn count_met(&self) -> usize {
        self.verdicts.iter().filter(|(_, v)| v.met).count()
    }

    /// How many members there are — `total(<group>)`.
    pub fn total(&self) -> usize {
        self.verdicts.len()
    }

    /// `count_met / total`, `0.0` for an empty group — `met_fraction(<group>)`.
    pub fn met_fraction(&self) -> f64 {
        if self.verdicts.is_empty() {
            0.0
        } else {
            self.count_met() as f64 / self.total() as f64
        }
    }

    /// At least `k` members are met — "k of n". A `k` above the group's size is never true.
    pub fn at_least(&self, k: usize) -> bool {
        self.count_met() >= k
    }

    /// Every member is met — `all_goals(<group>)`. An empty group is never "all met".
    pub fn all(&self) -> bool {
        !self.verdicts.is_empty() && self.count_met() == self.total()
    }

    /// At least one member is met.
    pub fn any(&self) -> bool {
        self.at_least(1)
    }

    /// The members that errored — "could not grade" is not "not met", so look before trusting a miss.
    pub fn errored(&self) -> Vec<&str> {
        self.verdicts.iter().filter(|(_, v)| v.error.is_some()).map(|(n, _)| n.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gv(mets: &[bool]) -> GroupVerdict {
        GroupVerdict {
            name: "q".into(),
            verdicts: mets.iter().enumerate().map(|(i, m)| (format!("j{i}"), Verdict::binary(*m))).collect(),
        }
    }

    /// The k-of-n acceptance row, plus all/any and the edges: an empty group and k > n.
    #[test]
    fn the_aggregates_count_over_the_members() {
        let two = gv(&[true, true, false]);
        assert!(two.at_least(2) && !two.at_least(3) && !two.at_least(4));
        assert!(!gv(&[true, false, false]).at_least(2));
        assert!(two.any() && !two.all());
        assert!(gv(&[true, true]).all());
        assert_eq!((two.count_met(), two.total()), (2, 3));
        assert!((two.met_fraction() - 2.0 / 3.0).abs() < 1e-9);
        let empty = gv(&[]);
        assert!(!empty.all() && !empty.any());
        assert_eq!(empty.met_fraction(), 0.0);
    }

    #[test]
    fn errored_members_are_named() {
        let mut v = gv(&[true]);
        v.verdicts.push(("broken".into(), Verdict::failed("boom")));
        assert_eq!(v.errored(), vec!["broken"]);
        let g = Group::new("q", [Judge::script("a", "a.sh"), Judge::script("b", "b.sh")]);
        assert_eq!((g.name(), g.member_names()), ("q", vec!["a".to_string(), "b".to_string()]));
    }
}
