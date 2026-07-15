//! `agg/state/verdicts.jsonl` — the durable, append-only record of every verdict the loop
//! RESOLVES (§5.8). Safety-critical GATE state, not an audit log: the gate's "was met" (§5.7)
//! reads it, so a failed write is a **hard error**, not a swallowed line.
//!
//! Nothing else records *when a judge last ran that stuck*: `state.json` is overwritten every
//! publish, `project.json` holds per-run rows with no verdicts, and `Goal.last_verdict` is
//! `#[serde(skip)]` — RAM only, reset to `Pending` every restart. This file is the from-zero
//! build that makes "was met" durable and cross-run.
//!
//! Each line is a NEW ENVELOPE wrapping a [`Verdict`] (which carries no `ts`/`session`/`step`),
//! stamped by the GATE with the session's final disposition. Only the **loop process** writes it
//! — `run.pid` already forbids a second loop, so there is no lock. `agg plan` and `agg judge`
//! write nothing: they are not the loop.

use crate::core::model::Verdict;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// What the GATE decided about the session that produced a verdict. Rows are stamped once the
/// session's fate is known (§5.8) — never at judge time. Only `Merged`/`Baseline` rows count as
/// "was met"; `RolledBack` describes code that did not land, `Staged` is unmerged span work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// the pre-session-1 pass against the untouched repo (§5.5.1) — the gate's seed for session 1.
    Baseline,
    /// the session's staged merge was kept.
    Merged,
    /// a previously-met judge regressed → the whole session's merge was discarded.
    RolledBack,
    /// a `skip_judges` span's verdicts while it is unmerged (§5.7/§5.8). Re-appended as `merged`
    /// when the span later merges; readers of "was met" and `stalled` ignore `staged` rows.
    Staged,
}

/// One line in `verdicts.jsonl`: a [`Verdict`] flattened into an envelope with the fields a
/// `Verdict` has no room for — `session`, `step`, `outcome`, `ts`. Field order matches §5.8.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictRecord {
    /// the session that produced it; `None` for the baseline pass (§5.5.1).
    pub session: Option<u32>,
    /// which step's session ran the judge (§5.4 step name).
    pub step: String,
    /// the judge / goal id.
    pub judge: String,
    pub met: bool,
    /// absent (skipped) preserves "the judge emitted no number" vs a real measured `0` — the
    /// distinction `model::Verdict` is careful about and `verdicts.jsonl` must keep faithfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    pub target: f64,
    /// set when the judge could not grade (§5.2). An errored row is recorded (the error text is
    /// the whole point of keeping the breakage loud) but is NEVER authoritative for "was met".
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub outcome: Outcome,
    /// real wall-clock epoch seconds (this is the loop process — clocks are fine here).
    pub ts: u64,
}

/// The gate's durable "was met" (§5.7): for each judge, whether its most recent LANDED, GRADED
/// verdict was met. A judge absent from the map has no such row — it was never met, so it cannot
/// regress. Reads ONLY `merged`/`baseline` rows and ignores errored ones: a rolled-back verdict
/// describes code that did not land, and "could not grade" (§5.2) is not "not met".
///
/// Read once per gate — cheap for a small log; a missing/rubbish file is honestly "no history",
/// never a crash (the log does not exist on the very first baseline write).
pub fn landed_met(dir: &Path) -> HashMap<String, bool> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(crate::paths::verdicts_jsonl(dir)) else {
        return out;
    };
    for line in text.lines() {
        let Ok(r) = serde_json::from_str::<VerdictRecord>(line) else { continue };
        // forward scan → the last matching row for a judge wins (the most recent).
        if r.error.is_none() && matches!(r.outcome, Outcome::Merged | Outcome::Baseline) {
            out.insert(r.judge, r.met);
        }
    }
    out
}

/// Append one row per verdict, stamped with `outcome` and `session` (`None` = baseline). `O_APPEND`
/// + **fsync per line**. Empty input is a no-op (no file is created).
///
/// A failed write is a HARD ERROR (returns `Err`): this is the gate's own seed, and a line that
/// never reached disk is a "was met" the gate will silently miss. This is deliberately the
/// OPPOSITE of `bus.rs::append_log`, which swallows every error and never fsyncs — correct for an
/// audit log, wrong for state the gate reads back.
///
/// Writer is the loop process ONLY (`run.pid` guarantees a single loop → no lock needed).
pub fn append(
    dir: &Path,
    session: Option<u32>,
    step: &str,
    verdicts: &[(String, Verdict)],
    outcome: Outcome,
) -> Result<()> {
    use std::io::Write;
    if verdicts.is_empty() {
        return Ok(());
    }
    let path = crate::paths::verdicts_jsonl(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let ts = crate::util::now_epoch();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    for (judge, v) in verdicts {
        let rec = VerdictRecord {
            session,
            step: step.to_string(),
            judge: judge.clone(),
            met: v.met,
            value: v.value,
            max: v.max,
            target: v.target,
            error: v.error.clone(),
            rationale: v.rationale.clone(),
            evidence: v.evidence.clone(),
            outcome,
            ts,
        };
        let line = serde_json::to_string(&rec).context("serializing verdict record")?;
        f.write_all(line.as_bytes())
            .and_then(|_| f.write_all(b"\n"))
            .with_context(|| format!("appending to {}", path.display()))?;
        // per-line durability: an un-fsync'd verdict the gate can't read back is worse than a
        // crash — it silently changes the decision. (bus.rs does the opposite; §5.8 says so.)
        f.sync_all().with_context(|| format!("fsync {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::Verdict;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "agg-verdicts-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn v(met: bool) -> Verdict {
        Verdict {
            met,
            value: Some(if met { 1.0 } else { 0.0 }),
            max: Some(1.0),
            target: 1.0,
            rationale: "r".into(),
            evidence: vec![],
            error: None,
        }
    }

    #[test]
    fn outcome_serializes_to_the_wire_names() {
        assert_eq!(serde_json::to_string(&Outcome::Baseline).unwrap(), "\"baseline\"");
        assert_eq!(serde_json::to_string(&Outcome::Merged).unwrap(), "\"merged\"");
        // NOT "rolledback" — a reader keys off the exact spelling.
        assert_eq!(serde_json::to_string(&Outcome::RolledBack).unwrap(), "\"rolled_back\"");
    }

    #[test]
    fn append_then_landed_met_roundtrips() {
        let d = tmpdir("roundtrip");
        append(&d, None, "baseline", &[("build".into(), v(true)), ("feat".into(), v(false))], Outcome::Baseline)
            .unwrap();
        let m = landed_met(&d);
        assert_eq!(m.get("build"), Some(&true));
        assert_eq!(m.get("feat"), Some(&false));
    }

    #[test]
    fn most_recent_merged_or_baseline_wins() {
        let d = tmpdir("recent");
        append(&d, None, "baseline", &[("g".into(), v(false))], Outcome::Baseline).unwrap();
        append(&d, Some(1), "worker", &[("g".into(), v(true))], Outcome::Merged).unwrap();
        assert_eq!(landed_met(&d).get("g"), Some(&true), "the newer merged row wins over baseline");
    }

    #[test]
    fn a_rolled_back_row_does_not_count_as_was_met() {
        // The poison the `outcome` field exists to prevent: a rolled-back session's not-met row
        // describes code that no longer exists. If it counted, the gate would compare the NEXT
        // session against a phantom.
        let d = tmpdir("rolledback");
        append(&d, None, "baseline", &[("build".into(), v(true))], Outcome::Baseline).unwrap();
        append(&d, Some(1), "worker", &[("build".into(), v(false))], Outcome::RolledBack).unwrap();
        assert_eq!(
            landed_met(&d).get("build"),
            Some(&true),
            "a rolled_back row must not overwrite the baseline 'was met'"
        );
    }

    #[test]
    fn an_errored_landed_row_is_not_authoritative() {
        // §5.2: a judge that could not grade said "I don't know", not "not met". Its row is kept
        // for the audit trail but must not flip 'was met' off.
        let d = tmpdir("errored");
        append(&d, None, "baseline", &[("build".into(), v(true))], Outcome::Baseline).unwrap();
        append(&d, Some(1), "worker", &[("build".into(), Verdict::failed("boom"))], Outcome::Merged).unwrap();
        assert_eq!(landed_met(&d).get("build"), Some(&true), "an errored merged row is ignored");
    }

    #[test]
    fn a_judge_with_no_landed_row_was_never_met() {
        // The freshness floor: never-met → cannot regress. No file, then only a rolled_back row.
        let d = tmpdir("never");
        assert!(!landed_met(&d).contains_key("ghost"), "no file → no history");
        append(&d, Some(1), "worker", &[("ghost".into(), v(false))], Outcome::RolledBack).unwrap();
        assert!(!landed_met(&d).contains_key("ghost"), "only a rolled_back row → still never met");
    }

    #[test]
    fn a_failed_write_is_a_hard_error_not_a_swallow() {
        // Put a FILE where the state dir must go so `agg/state/` cannot be created → the open
        // fails. Unlike bus.rs, append must PROPAGATE that, not swallow it: this is gate state.
        let d = tmpdir("hard");
        std::fs::write(d.join("agg"), "not a dir").unwrap();
        let r = append(&d, None, "baseline", &[("g".into(), v(true))], Outcome::Baseline);
        assert!(r.is_err(), "a verdicts.jsonl write failure must be a hard error");
    }

    #[test]
    fn empty_verdicts_writes_nothing() {
        let d = tmpdir("empty");
        append(&d, None, "baseline", &[], Outcome::Baseline).unwrap();
        assert!(!crate::paths::verdicts_jsonl(&d).exists(), "no verdicts → no file created");
    }

    #[test]
    fn absent_value_stays_absent_on_the_wire() {
        // A numberless judge ({"met":true}) must NOT invent a 0 on disk — the same faithfulness
        // model::Verdict guarantees.
        let d = tmpdir("absent");
        let numberless = Verdict {
            met: true,
            value: None,
            max: None,
            target: 1.0,
            rationale: String::new(),
            evidence: vec![],
            error: None,
        };
        append(&d, None, "baseline", &[("g".into(), numberless)], Outcome::Baseline).unwrap();
        let line = std::fs::read_to_string(crate::paths::verdicts_jsonl(&d)).unwrap();
        assert!(!line.contains("value"), "absent value must not serialize: {line}");
        assert!(!line.contains("max"), "absent max must not serialize: {line}");
    }
}
