//! The evidence feature: after judging, keep the files each verdict cites, and hand what was kept to
//! the verdict ledger and the run report (`core::evidence` does the copying).
//!
//! Called where a verdict is PRODUCED — the baseline pass, `RunJudges`, the driver's `run_judge` —
//! because that is the one moment the judge's scratch is known to be the right one: a driver may
//! gate a verdict sessions after it was judged, and by then the session's scratch is another one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::context::LoopState;
use crate::core::evidence::{self, Budget, Kept, Roots};
use crate::core::model::Verdict;

/// The run's evidence state, in the run's `ext` store.
#[derive(Default)]
pub struct EvidenceState {
    /// this run's id — the directory level that keeps two runs' session numbers apart.
    run_id: Option<String>,
    /// the run-wide byte budget, built from `judge.evidence_max_mb` on first use.
    budget: Option<Budget>,
    /// per judge: the verdict whose evidence was kept last, and what became of it. The verdict is
    /// kept so a reader pairs the copies with THAT verdict only — a rollback restores an older one.
    last: HashMap<String, (Verdict, Kept)>,
}

/// Start the run's evidence directory under `run_id`. Both entry points call it once, at boot.
pub fn begin(ctx: &mut LoopState, run_id: &str) {
    ctx.ext.get::<EvidenceState>().run_id = Some(run_id.to_string());
}

/// `agg/private/evidence/<run>/` — this run's evidence.
fn run_dir(ctx: &mut LoopState) -> PathBuf {
    let run = ctx.ext.get::<EvidenceState>().run_id.clone().unwrap_or_else(|| "run".into());
    crate::paths::evidence_dir(&ctx.dir).join(run)
}

/// Keep the evidence of every verdict in `fresh`, judged in `session` (`None` = the baseline pass).
/// Never fails: what cannot be kept is recorded with its reason and warned about.
/// `tier` is the one the judges ran in — it decides where their scratch was.
pub fn keep(ctx: &mut LoopState, session: Option<u32>, fresh: &[(String, Verdict)], tier: crate::isolation::Isolation) {
    let cap = ctx.cfg.judge.evidence_max_mb;
    let roots = Roots { scratch: crate::core::judge::scratch_path(&ctx.dir, session, tier), project: ctx.dir.clone() };
    let label = session.map_or_else(|| "baseline".to_string(), |s| s.to_string());
    let base = run_dir(ctx).join(label);
    let st = ctx.ext.get::<EvidenceState>();
    let budget = st.budget.get_or_insert_with(|| Budget::mb(cap));
    for (judge, v) in fresh {
        if v.evidence.is_empty() {
            st.last.remove(judge);
            continue;
        }
        let was_hit = budget.hit;
        let kept = evidence::persist(&v.evidence, &roots, &base.join(evidence::dir_name(judge)), budget);
        for why in &kept.skipped {
            eprintln!("  ⚠ evidence of `{judge}` not kept — {why}");
        }
        if budget.hit && !was_hit {
            eprintln!("  ⚠ evidence size cap reached ({cap} MB this run, `judge.evidence_max_mb`) — the run goes on");
        }
        st.last.insert(judge.clone(), (v.clone(), kept));
    }
}

/// The kept evidence of `v`, if `v` is the verdict it was kept for: `(saved, skipped)`.
fn kept_for<'a>(st: &'a EvidenceState, judge: &str, v: &Verdict) -> Option<&'a Kept> {
    st.last.get(judge).filter(|(was, _)| was == v).map(|(_, k)| k)
}

/// For the verdict ledger: per judge in `fresh`, its kept copies (project-relative) and skips.
pub fn for_ledger(ctx: &mut LoopState, fresh: &[(String, Verdict)]) -> HashMap<String, (Vec<String>, Vec<String>)> {
    let dir = ctx.dir.clone();
    let st = ctx.ext.get::<EvidenceState>();
    fresh
        .iter()
        .filter_map(|(judge, v)| {
            let k = kept_for(st, judge, v)?;
            let saved = k.saved.iter().map(|p| relative(&dir, p)).collect();
            Some((judge.clone(), (saved, k.skipped.clone())))
        })
        .collect()
}

/// For the report: the kept copies of `v` (as `judge`'s verdict), as given — absolute when `dir` is.
pub fn saved_for(ctx: &mut LoopState, judge: &str, v: &Verdict) -> Vec<PathBuf> {
    kept_for(ctx.ext.get::<EvidenceState>(), judge, v).map(|k| k.saved.clone()).unwrap_or_default()
}

/// Every judge's last kept verdict and its copies — the report pairs each with the verdict it shows.
pub fn all_saved(ctx: &mut LoopState) -> HashMap<String, (Verdict, Vec<PathBuf>)> {
    let st = ctx.ext.get::<EvidenceState>();
    st.last.iter().map(|(j, (v, k))| (j.clone(), (v.clone(), k.saved.clone()))).collect()
}

/// This run's evidence directory, once anything was kept in it.
pub fn run_dir_if_used(ctx: &mut LoopState) -> Option<PathBuf> {
    Some(run_dir(ctx)).filter(|d| d.is_dir())
}

/// `p` relative to the project, for the ledger — the project may move, the layout does not.
fn relative(dir: &Path, p: &Path) -> String {
    p.strip_prefix(dir).unwrap_or(p).to_string_lossy().into_owned()
}
