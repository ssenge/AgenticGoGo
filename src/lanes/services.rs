//! What the lanes of one run share: one instance of the engine runs in each lane, and these are the things that must be
//! the run's, not the lane's: the stop flag, the gate a rate limit closes, the run's spend and the ceilings on it, and
//! what the views show of each lane.
//!
//! [`LaneServices`] is what an engine instance calls; [`Shared`] is its thread-safe implementation. An instance holds an
//! `Option<Arc<dyn LaneServices>>`: `None` is an ordinary run, and every call site keeps its own code then.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Where a lane is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneState {
    /// not started: waiting for room under `max`
    Queued,
    Running,
    Done,
    Failed,
    /// ended on a stop, resumable
    Stopped,
}

/// What a view shows of one lane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneView {
    pub key: String,
    pub step: String,
    pub state: LaneState,
    pub tokens: u64,
    pub cost: f64,
    /// waiting out a rate limit
    pub waiting: bool,
    pub started: Option<u64>,
    pub ended: Option<u64>,
}

impl LaneView {
    /// A lane that has not started.
    pub fn queued(key: &str) -> LaneView {
        LaneView { key: key.into(), step: String::new(), state: LaneState::Queued, tokens: 0, cost: 0.0, waiting: false, started: None, ended: None }
    }
}

/// What an engine instance in a lane calls.
pub trait LaneServices: Send + Sync {
    /// The run is ending (a stop, a ceiling, a failed lane under `fail_fast`): a lane starts nothing new.
    fn stop_requested(&self) -> bool;
    /// Blocks while the gate of `agent` is closed (a lane met its rate limit); returns when it opens or a stop is asked.
    fn wait_rate(&self, agent: &str);
    /// A lane met `agent`'s rate limit, reset at `until` (epoch seconds): no lane launches a session of it before.
    fn rate_hit(&self, agent: &str, until: u64);
    /// A lane's session spent this much (output tokens, millionths of a dollar).
    fn charge(&self, tokens: u64, cost_micros: u64);
    /// The ceiling the run's total spend broke, if one.
    fn ceiling_hit(&self) -> Option<String>;
    /// A lane's view, as it is now.
    fn view(&self, view: LaneView);
}

/// The ceilings on the whole run's spend.
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    pub tokens: Option<u64>,
    pub cost: Option<f64>,
}

/// The thread-safe implementation, shared by the scheduler and every lane.
pub struct Shared {
    stop: Mutex<Option<String>>,
    gate: Mutex<HashMap<String, u64>>,
    opened: Condvar,
    spent: Mutex<(u64, u64)>,
    limits: Limits,
    views: Mutex<BTreeMap<String, LaneView>>,
    paused: std::sync::atomic::AtomicBool,
}

impl Shared {
    /// Services for one run's lanes, under `limits`.
    pub fn new(limits: Limits) -> Arc<Shared> {
        Arc::new(Shared {
            stop: Mutex::new(None),
            gate: Mutex::new(HashMap::new()),
            opened: Condvar::new(),
            spent: Mutex::new((0, 0)),
            limits,
            views: Mutex::new(BTreeMap::new()),
            paused: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The operator paused the run: no lane starts until it resumes (running lanes get the command on their own bus).
    pub fn set_paused(&self, on: bool) {
        self.paused.store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the run is paused.
    pub fn paused(&self) -> bool {
        self.paused.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Asks the lanes to stop, with why; the first reason stays.
    pub fn request_stop(&self, why: &str) {
        self.stop.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(|| why.to_string());
        self.opened.notify_all();
    }

    /// Why a stop was asked, if one was.
    pub fn stop_reason(&self) -> Option<String> {
        self.stop.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The run's spend so far: output tokens and dollars.
    pub fn spent(&self) -> (u64, f64) {
        let (t, c) = *self.spent.lock().unwrap_or_else(|e| e.into_inner());
        (t, c as f64 / 1_000_000.0)
    }

    /// Every lane's view, by key.
    pub fn views(&self) -> Vec<LaneView> {
        self.views.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }

    /// The latest reset of a closed gate (epoch seconds), if any is closed at `now`.
    pub fn gate_until(&self, now: u64) -> Option<u64> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner()).values().copied().filter(|u| *u > now).max()
    }
}

impl LaneServices for Shared {
    fn stop_requested(&self) -> bool {
        self.stop_reason().is_some()
    }

    fn wait_rate(&self, agent: &str) {
        let mut gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let until = gate.get(agent).copied().unwrap_or(0);
            let now = crate::util::now_epoch();
            if until <= now || self.stop_requested() {
                return;
            }
            // woken when the gate moves or a stop comes, else at the reset (a second at most: a stop may not notify)
            let wait = Duration::from_secs((until - now).min(1));
            gate = self.opened.wait_timeout(gate, wait).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn rate_hit(&self, agent: &str, until: u64) {
        let mut gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let held = gate.entry(agent.to_string()).or_insert(0);
        *held = (*held).max(until);
        self.opened.notify_all();
    }

    fn charge(&self, tokens: u64, cost_micros: u64) {
        let (t, c) = {
            let mut s = self.spent.lock().unwrap_or_else(|e| e.into_inner());
            s.0 += tokens;
            s.1 += cost_micros;
            (s.0, s.1)
        };
        if let Some(max) = self.limits.tokens.filter(|m| t >= *m) {
            self.request_stop(&format!("limits.tokens: {t}/{max}"));
        }
        if let Some(max) = self.limits.cost.filter(|m| c as f64 / 1_000_000.0 >= *m) {
            self.request_stop(&format!("limits.cost: ${:.2}/${max:.2}", c as f64 / 1_000_000.0));
        }
    }

    fn ceiling_hit(&self) -> Option<String> {
        self.stop_reason().filter(|r| r.starts_with("limits."))
    }

    fn view(&self, view: LaneView) {
        self.views.lock().unwrap_or_else(|e| e.into_inner()).insert(view.key.clone(), view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Stop and its reason: the first reason stays; a ceiling is a stop that names a limit.
    #[test]
    fn a_stop_keeps_its_first_reason_and_a_ceiling_is_a_stop_that_names_a_limit() {
        let s = Shared::new(Limits { tokens: Some(100), cost: None });
        assert!(!s.stop_requested() && s.ceiling_hit().is_none());
        s.request_stop("the operator");
        s.request_stop("later");
        assert_eq!(s.stop_reason().as_deref(), Some("the operator"));
        assert!(s.ceiling_hit().is_none(), "a stop of the operator's is no ceiling");
        let c = Shared::new(Limits { tokens: Some(100), cost: Some(1.0) });
        c.charge(60, 0);
        assert!(!c.stop_requested());
        c.charge(60, 0);
        assert_eq!(c.ceiling_hit().as_deref(), Some("limits.tokens: 120/100"));
        let d = Shared::new(Limits { tokens: None, cost: Some(0.5) });
        d.charge(1, 600_000);
        assert!(d.ceiling_hit().unwrap().starts_with("limits.cost"));
    }

    /// Charges from eight threads add up exactly.
    #[test]
    fn charges_from_eight_threads_add_up() {
        let s = Shared::new(Limits::default());
        std::thread::scope(|sc| {
            for _ in 0..8 {
                sc.spawn(|| (0..500).for_each(|_| s.charge(2, 1000)));
            }
        });
        let (tokens, cost) = s.spent();
        assert_eq!(tokens, 8 * 500 * 2);
        assert!((cost - 8.0 * 500.0 * 0.001).abs() < 1e-9);
    }

    /// A closed gate holds a second waiter until the reset; another agent's gate is open; a stop ends the wait.
    #[test]
    fn a_rate_gate_holds_a_second_waiter_until_the_reset_and_a_stop_ends_it() {
        let s = Shared::new(Limits::default());
        let now = crate::util::now_epoch();
        s.rate_hit("claude", now + 2);
        let started = std::time::Instant::now();
        s.wait_rate("codex");
        assert!(started.elapsed() < Duration::from_millis(500), "another agent's gate is open");
        let waited = AtomicUsize::new(0);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                s.wait_rate("claude");
                waited.fetch_add(1, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(waited.load(Ordering::SeqCst), 0, "still waiting at the gate");
        });
        assert!(started.elapsed() >= Duration::from_millis(800), "the waiter went through only at the reset");
        s.rate_hit("claude", crate::util::now_epoch() + 60);
        let t = std::time::Instant::now();
        std::thread::scope(|sc| {
            sc.spawn(|| {
                std::thread::sleep(Duration::from_millis(200));
                s.request_stop("stop");
            });
            s.wait_rate("claude");
        });
        assert!(t.elapsed() < Duration::from_secs(5), "a stop ends the wait at a closed gate");
    }

    /// Views are kept by lane key, newest wins.
    #[test]
    fn views_are_kept_by_key() {
        let s = Shared::new(Limits::default());
        s.view(LaneView::queued("b"));
        s.view(LaneView::queued("a"));
        s.view(LaneView { state: LaneState::Running, step: "build".into(), ..LaneView::queued("a") });
        let v = s.views();
        assert_eq!(v.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!((v[0].state, v[0].step.as_str()), (LaneState::Running, "build"));
    }
}
