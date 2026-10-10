//! The rate-limit wait (A-18): a rate-limited session is INCOMPLETE, so the loop saves its work,
//! waits for the limit to reset, and goes round again.
//!
//! The wait is a state of a RUNNING workflow, not a pause in it (decision 6):
//! - it is VISIBLE — `waiting: {reason, agent, until}` in `state.json` / `agg status --json`, a row
//!   in `agg/private/waits.jsonl` when it starts and another when it ends;
//! - it DRAINS THE BUS — `agg send wake` (or `resume`) ends it now, `agg stop` ends the run;
//! - it is NOT WORK and NOT A LIMIT — its seconds never count against `work_time`, and nothing about
//!   waiting ends the run. Ceilings already tripped (a deadline) still end it.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::bus::Command;
use crate::loop_::{AGGScratch, Flow, Handler, LifecycleEvent, LoopState, RunOutcome};
use crate::state::{Waiting, RATE_LIMIT};

/// Slack added to a reported reset time, so the next attempt lands after the boundary, not on it.
const RESET_BUFFER_SECS: u64 = 90;
/// How often the wait looks at the bus, the signals and the ceilings. Short, so a wake starts the
/// next session within seconds.
const POLL: Duration = Duration::from_secs(1);

/// How a wait ended — the `how` of its closing ledger row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitEnd {
    /// the reset time (or the fixed backoff) passed.
    Elapsed,
    /// `agg send wake` / `agg send resume`.
    Woken,
    /// `agg stop` / `agg send stop`.
    Stopped,
    /// SIGINT / SIGTERM.
    Interrupted,
    /// an `abort_if` ceiling tripped while waiting.
    Ceiling,
}

/// One row of `agg/private/waits.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WaitRow {
    Wait {
        session: u32,
        #[serde(flatten)]
        waiting: Waiting,
        /// where the rate-limited session's unjudged work was saved, if it left any.
        partial: Option<String>,
        ts: u64,
    },
    WaitEnd { session: u32, how: WaitEnd, waited_s: u64, ts: u64 },
}

/// When a wait ends on its own: the reported reset plus a buffer, else the fixed backoff.
pub fn wait_until(now: u64, reset: Option<u64>, backoff_secs: u64) -> u64 {
    match reset {
        Some(r) => r.max(now) + RESET_BUFFER_SECS,
        None => now + backoff_secs,
    }
}

/// Whether a wait that began at `since` is over at `now`: its time is up, or a wake arrived and the
/// minimum gap between two attempts has passed.
pub fn wait_is_over(now: u64, since: u64, until: u64, woken: bool, min_gap: u64) -> bool {
    now >= until || (woken && now >= since.saturating_add(min_gap))
}

/// What the operator asked of a waiting loop.
#[derive(Debug, PartialEq)]
enum Signal {
    Wake,
    Stop(String),
}

/// Sort one drained batch: a stop wins over everything, a wake/resume ends the wait, and every
/// other command is kept for the next session boundary, where `BusDrain` applies it as usual.
fn sort_commands(cmds: Vec<Command>, deferred: &mut Vec<Command>) -> Option<Signal> {
    let mut signal = None;
    for cmd in cmds {
        match cmd {
            Command::Stop { reason } => return Some(Signal::Stop(reason)),
            Command::Wake | Command::Resume => signal = Some(Signal::Wake),
            other => deferred.push(other),
        }
    }
    signal
}

/// The wait itself, as an `on_verify` step right after the worker. Plain rate limit →
/// `SkipSession` (no judging, no gate); a stop, an interrupt or a tripped ceiling → `Stop`.
pub struct RateLimitBackoff;
impl Handler for RateLimitBackoff {
    fn run(&self, ctx: &mut LoopState) -> Result<Flow> {
        let Some(reset) = rate_limit(ctx) else { return Ok(Flow::Continue) };
        let since = crate::util::now_epoch();
        let until = wait_until(since, reset, ctx.cfg.ratelimit_backoff_secs);
        let secs = until - since;
        match reset {
            Some(_) => eprintln!("  rate limit detected — waiting {}m{:02}s until it resets", secs / 60, secs % 60),
            None => eprintln!("  rate limit detected — backing off {secs}s (no reset time reported)"),
        }
        eprintln!("  (`agg send wake` ends the wait early; `agg stop` ends the run)");
        if ctx.cfg.memory.enabled {
            crate::core::memory::clear_scratch(&ctx.dir, ctx.session);
        }
        let partial = crate::features::partial::save_and_reset(ctx);
        let agent = ctx.cur_step.as_ref().map(|s| s.agent.clone()).unwrap_or_default();
        let waiting = Waiting { reason: RATE_LIMIT.into(), agent, until };
        append(ctx, &WaitRow::Wait { session: ctx.session, waiting: waiting.clone(), partial, ts: since });
        ctx.dash.waiting = Some(waiting);
        ctx.emit(LifecycleEvent::Backoff);

        let (how, stop) = wait_out(ctx, since, until);
        let now = crate::util::now_epoch();
        ctx.clock.add_rate_limit_wait(&ctx.dir, now.saturating_sub(since) as f64);
        append(ctx, &WaitRow::WaitEnd { session: ctx.session, how, waited_s: now.saturating_sub(since), ts: now });
        if let Some(stop) = stop {
            // the final snapshot and the report keep `waiting`: the run ended while waiting.
            return Ok(Flow::Stop(stop(ctx)));
        }
        ctx.dash.waiting = None;
        ctx.publish();
        Ok(Flow::SkipSession)
    }
    fn name(&self) -> &'static str {
        "RateLimitBackoff"
    }
}

/// `Some(reset)` when the session that just ended was rate-limited (`reset` = the reported reset
/// epoch, if the agent named one); `None` for a normal session.
fn rate_limit(ctx: &mut LoopState) -> Option<Option<u64>> {
    let o = ctx.scratch.get::<AGGScratch>().outcome.as_ref()?;
    o.rate_limited.then_some(o.rate_limit_reset)
}

/// How a run that stops during the wait is finished — deferred so the ledger row and the banked wait
/// are written before the run's final snapshot.
type Finish = Box<dyn FnOnce(&mut LoopState) -> RunOutcome>;

/// Wait until the limit resets, a wake arrives (no sooner than the minimum gap), or the run ends.
fn wait_out(ctx: &mut LoopState, since: u64, until: u64) -> (WaitEnd, Option<Finish>) {
    let mut deferred = Vec::new();
    let mut woken = false;
    let end: (WaitEnd, Option<Finish>) = loop {
        if crate::os::signals::interrupted() {
            break (WaitEnd::Interrupted, Some(Box::new(|c: &mut LoopState| c.finish_interrupted())));
        }
        let rs = ctx.run_state();
        let ceil = ctx.eng.conditions_only(&rs);
        if ceil.halt {
            // ponytail: aborts without firing `notify.cmd` (STUCK_NOTIFY §12.6) — the operator's
            // next question is about quota. Upgrade: `notify::halt_ping` before `abort_now`.
            let reason = format!("abort_if: {}", ceil.halt_reason.unwrap_or_default());
            eprintln!("  ⚠ ceiling tripped during the rate-limit wait — aborting");
            break (WaitEnd::Ceiling, Some(Box::new(move |c: &mut LoopState| c.abort_now(&reason))));
        }
        let cmds = ctx.bus.as_ref().map(|b| b.drain()).unwrap_or_default();
        match sort_commands(cmds, &mut deferred) {
            Some(Signal::Stop(reason)) => {
                break (WaitEnd::Stopped, Some(Box::new(move |c: &mut LoopState| c.stopped_via_bus(reason))));
            }
            Some(Signal::Wake) if !woken => {
                woken = true;
                let at = since.saturating_add(ctx.cfg.ratelimit_min_gap_secs);
                let left = at.saturating_sub(crate::util::now_epoch());
                eprintln!("  [bus] wake → ending the rate-limit wait (next attempt in {left}s)");
            }
            _ => {}
        }
        if wait_is_over(crate::util::now_epoch(), since, until, woken, ctx.cfg.ratelimit_min_gap_secs) {
            break (if woken { WaitEnd::Woken } else { WaitEnd::Elapsed }, None);
        }
        std::thread::sleep(POLL);
    };
    requeue(ctx, deferred);
    end
}

/// Put the commands a wait does not act on back on the bus, ahead of anything sent later, so the
/// next session boundary applies them exactly as if no wait had happened.
fn requeue(ctx: &LoopState, deferred: Vec<Command>) {
    let Some(bus) = &ctx.bus else { return };
    for cmd in deferred {
        if let Err(e) = bus.send(&cmd, "0000000000000") {
            eprintln!("  ⚠ [bus] could not keep {cmd:?} for the next session: {e}");
        }
    }
}

/// Append one row to the wait ledger. An audit record, not gate state: a failed write is reported
/// and the run goes on.
fn append(ctx: &LoopState, row: &WaitRow) {
    use std::io::Write;
    let path = crate::paths::waits_jsonl(&ctx.dir);
    let line = serde_json::to_string(row).unwrap_or_default();
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = res {
        eprintln!("  ⚠ could not record the wait in {}: {e}", path.display());
    }
}

/// Every row of the wait ledger, oldest first. A missing file is no waits.
pub fn rows(dir: &std::path::Path) -> Vec<WaitRow> {
    std::fs::read_to_string(crate::paths::waits_jsonl(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reported reset wins, plus the buffer; one already in the past waits only the buffer;
    /// none at all falls back to the fixed backoff.
    #[test]
    fn the_wait_ends_after_the_reset_or_the_fixed_backoff() {
        assert_eq!(wait_until(1_000, Some(4_600), 1_800), 4_600 + RESET_BUFFER_SECS);
        assert_eq!(wait_until(1_000, Some(10), 1_800), 1_000 + RESET_BUFFER_SECS);
        assert_eq!(wait_until(1_000, None, 1_800), 2_800);
    }

    /// A-18.2: a wake ends the wait — but never sooner than the minimum gap after the limited
    /// attempt, so an eager operator cannot hammer the provider. Without a wake only time ends it.
    #[test]
    fn a_wake_ends_the_wait_once_the_minimum_gap_has_passed() {
        let (since, until, gap) = (1_000, 4_690, 60);
        assert!(!wait_is_over(1_010, since, until, false, gap), "no wake, no reset: keep waiting");
        assert!(!wait_is_over(1_010, since, until, true, gap), "woken, but inside the gap");
        assert!(wait_is_over(1_060, since, until, true, gap), "woken and the gap is over");
        assert!(!wait_is_over(1_060, since, until, false, gap), "the gap alone does not end a wait");
        assert!(wait_is_over(4_690, since, until, false, gap), "the reset passed");
    }

    /// The wait acts on wake/resume and stop only; everything else is kept, in order, for the next
    /// session boundary — and a stop beats a wake in the same batch.
    #[test]
    fn the_wait_sorts_the_bus_into_wake_stop_and_later() {
        let mut later = Vec::new();
        let got = sort_commands(
            vec![Command::Note { text: "n".into() }, Command::Resume, Command::Pause],
            &mut later,
        );
        assert_eq!(got, Some(Signal::Wake), "`resume` while waiting is a wake");
        assert!(matches!(later.as_slice(), [Command::Note { .. }, Command::Pause]), "{later:?}");

        let mut later = Vec::new();
        let got = sort_commands(vec![Command::Wake, Command::Stop { reason: "kill".into() }], &mut later);
        assert_eq!(got, Some(Signal::Stop("kill".into())));
        assert_eq!(sort_commands(vec![], &mut later), None);
    }

    /// The ledger row is self-describing: the event, the wait's three public fields at top level,
    /// and how it ended.
    #[test]
    fn wait_rows_round_trip_with_flat_fields() {
        let w = Waiting { reason: RATE_LIMIT.into(), agent: "claude".into(), until: 9 };
        let row = WaitRow::Wait { session: 2, waiting: w, partial: Some("refs/agg/partial/r/2".into()), ts: 1 };
        let j = serde_json::to_value(&row).unwrap();
        assert_eq!(j["event"], "wait");
        assert_eq!(j["reason"], "rate_limit");
        assert_eq!(j["until"], 9);
        assert_eq!(serde_json::from_value::<WaitRow>(j).unwrap(), row);
        let end = serde_json::to_value(WaitRow::WaitEnd { session: 2, how: WaitEnd::Woken, waited_s: 3, ts: 4 }).unwrap();
        assert_eq!(end["event"], "wait_end");
        assert_eq!(end["how"], "woken");
    }
}
