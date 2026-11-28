//! Launching and supervising lanes: up to `max` run at once, each on a scoped thread of its own, started in item order;
//! about every second the caller's `control` runs (it forwards the operator's commands to the lanes and mirrors their
//! asks). A lane that panics is a failed lane, not a dead run.

use super::services::Shared;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// What to run.
#[derive(Debug, Clone)]
pub struct Plan {
    pub keys: Vec<String>,
    /// how many lanes run at once (at least 1)
    pub max: usize,
    /// a failed lane stops the running ones and starts no more
    pub fail_fast: bool,
}

/// How the scheduler reads a lane's result: whether it is a failed lane, and what a panic's message makes of one.
pub struct Reading<'a, R> {
    pub failed: &'a (dyn Fn(&R) -> bool + Sync),
    pub panicked: &'a (dyn Fn(String) -> R + Sync),
}

/// How often the caller's `control` runs while lanes do.
const TICK: Duration = Duration::from_millis(500);

/// Runs `body(index, key)` for the keys of `plan`, at most `plan.max` at a time, starting them in order; `failed` says
/// `reading` says whether a result is a failed lane and makes one of a panic's message. The results are in key order: `None`
/// is a lane that was never started (a stop, or `fail_fast`, came first).
pub fn run<R: Send>(
    plan: &Plan,
    services: &Arc<Shared>,
    control: &(dyn Fn(&[usize]) + Sync),
    body: &(dyn Fn(usize, &str) -> R + Sync),
    reading: &Reading<R>,
) -> Vec<Option<R>> {
    let n = plan.keys.len();
    let mut results: Vec<Option<R>> = (0..n).map(|_| None).collect();
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<(usize, R)>();
        let (mut next, mut running, mut any_failed) = (0, Vec::<usize>::new(), false);
        loop {
            while running.len() < plan.max.max(1) && next < n && !services.stop_requested() && !services.paused() && !(plan.fail_fast && any_failed) {
                let i = next;
                next += 1;
                running.push(i);
                let tx = tx.clone();
                scope.spawn(move || {
                    let r = catch_unwind(AssertUnwindSafe(|| body(i, &plan.keys[i]))).unwrap_or_else(|p| (reading.panicked)(panic_text(&p)));
                    let _ = tx.send((i, r));
                });
            }
            if running.is_empty() {
                // paused with lanes waiting: the operator's resume (or stop) is what ends this
                if next < n && services.paused() && !services.stop_requested() {
                    std::thread::sleep(TICK);
                    control(&running);
                    continue;
                }
                break;
            }
            if let Ok((i, r)) = rx.recv_timeout(TICK) {
                running.retain(|x| *x != i);
                if (reading.failed)(&r) {
                    any_failed = true;
                    if plan.fail_fast {
                        services.request_stop(&format!("lane `{}` failed (fail_fast)", plan.keys[i]));
                    }
                }
                results[i] = Some(r);
            }
            control(&running);
        }
    });
    results
}

/// A panic's message.
fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "a panic".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::services::Limits;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn plan(n: usize, max: usize, fail_fast: bool) -> Plan {
        Plan { keys: (0..n).map(|i| format!("k{i}")).collect(), max, fail_fast }
    }

    type R = Result<usize, String>;

    fn go(plan: &Plan, body: &(dyn Fn(usize, &str) -> R + Sync)) -> (Vec<Option<R>>, Arc<Shared>) {
        let services = Shared::new(Limits::default());
        let results = run(plan, &services, &|_| {}, body, &Reading { failed: &|r: &R| r.is_err(), panicked: &|p| Err(format!("panic: {p}")) });
        (results, services)
    }

    /// Two 1-second lanes at max 2 take about one second; at max 1 about two; the results come back in key order.
    #[test]
    fn lanes_run_at_once_up_to_max_and_results_are_in_key_order() {
        let sleep = |i: usize, _: &str| -> R {
            std::thread::sleep(Duration::from_millis(700));
            Ok(i)
        };
        let t = Instant::now();
        let (results, _) = go(&plan(2, 2, false), &sleep);
        assert!(t.elapsed() < Duration::from_millis(1300), "{:?}", t.elapsed());
        assert_eq!(results.into_iter().map(|r| r.unwrap().unwrap()).collect::<Vec<_>>(), [0, 1]);
        let t = Instant::now();
        let _ = go(&plan(2, 1, false), &sleep);
        assert!(t.elapsed() >= Duration::from_millis(1400), "max 1 serialises: {:?}", t.elapsed());
    }

    /// Five lanes at max 2 never have three at once.
    #[test]
    fn never_more_than_max_at_once() {
        let (now, peak) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let (results, _) = go(&plan(5, 2, false), &|i, _| {
            let c = now.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(c, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(150));
            now.fetch_sub(1, Ordering::SeqCst);
            Ok(i)
        });
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert!(results.iter().all(|r| r.is_some()));
    }

    /// `fail_fast`: a failed lane stops the services and the lanes after it are never started; off, the others finish.
    #[test]
    fn fail_fast_starts_no_more_lanes_and_without_it_the_others_finish() {
        let body = |i: usize, _: &str| -> R {
            std::thread::sleep(Duration::from_millis(100));
            if i == 0 { Err("boom".into()) } else { Ok(i) }
        };
        let (results, services) = go(&plan(4, 1, true), &body);
        assert!(results[0].as_ref().unwrap().is_err() && results[1..].iter().all(|r| r.is_none()), "{results:?}");
        assert!(services.stop_reason().unwrap().contains("`k0` failed"));
        let (results, services) = go(&plan(4, 1, false), &body);
        assert!(results.iter().all(|r| r.is_some()) && !services.stop_requested());
    }

    /// A stop before the start launches nothing; a panicking lane is a failed lane.
    #[test]
    fn a_stop_launches_nothing_and_a_panic_is_a_failed_lane() {
        let services = Shared::new(Limits::default());
        services.request_stop("operator");
        let p = plan(3, 2, false);
        let r = run(&p, &services, &|_| {}, &|i, _| -> R { Ok(i) }, &Reading { failed: &|r: &R| r.is_err(), panicked: &|m| Err(m) });
        assert!(r.iter().all(|x| x.is_none()));
        let (results, _) = go(&plan(2, 2, false), &|i, _| if i == 1 { panic!("lane blew up") } else { Ok(i) });
        assert_eq!(results[1].as_ref().unwrap().as_ref().unwrap_err(), "panic: lane blew up");
        assert!(results[0].as_ref().unwrap().is_ok());
    }

    /// The control runs while lanes do, with the indices that run.
    #[test]
    fn the_control_runs_while_lanes_do() {
        let ticks = AtomicUsize::new(0);
        let services = Shared::new(Limits::default());
        let _ = run(&plan(1, 1, false), &services, &|running| {
            assert_eq!(running.len().min(1), running.len());
            ticks.fetch_add(1, Ordering::SeqCst);
        }, &|i, _| -> R {
            std::thread::sleep(Duration::from_millis(1200));
            Ok(i)
        }, &Reading { failed: &|r: &R| r.is_err(), panicked: &|m| Err(m) });
        assert!(ticks.load(Ordering::SeqCst) >= 1);
    }
}
