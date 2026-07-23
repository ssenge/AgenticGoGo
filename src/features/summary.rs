//! The LLM summarizer feature — a best-effort plugin on `on_session_end` that keeps the rolling
//! cumulative/windowed summary and feeds `RefineFold` via `AGGScratch::summarized_this_cycle`.

use std::time::Instant;

use anyhow::Result;

use crate::loop_::{AGGScratch, AGGState, Flow, Handler, LoopState};
use crate::summary;

pub struct Summarize;
impl Handler for Summarize {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let min_interval = ctx.cfg.summary.min_interval_secs;
        let due = ctx
            .ext
            .get::<AGGState>()
            .summary
            .last_summary
            .map(|t| t.elapsed().as_secs() >= min_interval)
            .unwrap_or(true); // None only before Setup primes it — treat as due (matches old init)
        if !(ctx.cfg.summary.enabled && due) {
            return Ok(Flow::Continue);
        }
        let model = ctx.ruler.default_summary_model().to_string();
        let thoughts = ctx.scratch.get::<AGGScratch>().outcome.as_ref().map(|o| o.thoughts.clone()).unwrap_or_default();
        let deltas = ctx.scratch.get::<AGGScratch>().res.as_ref().map(|r| r.deltas.clone()).unwrap_or_default();
        let cumulative = ctx.ext.get::<AGGState>().summary.cumulative.clone();
        if let Some((s, spend)) = summary::summarize(ctx.ruler, &model, &cumulative, &thoughts, &deltas, 120) {
            eprintln!("  [SUMMARY cumulative] {}", s.cumulative);
            eprintln!("  [SUMMARY windowed]   {}", s.windowed);
            ctx.ext.get::<AGGState>().summary.cumulative = s.cumulative.clone();
            ctx.dash.summary_cumulative = s.cumulative;
            ctx.dash.summary_windowed = s.windowed;
            ctx.ext.get::<AGGState>().summary.last_summary = Some(Instant::now());
            ctx.scratch.get::<AGGScratch>().summarized_this_cycle = true;
            // §5.6: summarizer spend counts too — the summarizer runs on the ruler.
            ctx.tokens_spent += spend.tokens;
            if let Some(c) = spend.cost_usd {
                ctx.cost_spent += c;
            }
            let ruler_agent = ctx.cfg.judge.agent.clone();
            ctx.charge(&ruler_agent, spend.tokens, spend.cost_usd);
            ctx.publish();
        }
        Ok(Flow::Continue)
    }
    fn name(&self) -> &'static str {
        "Summarize"
    }
}
