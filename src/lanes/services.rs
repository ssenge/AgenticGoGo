//! What the lanes of one run share: one instance of the engine runs in each lane, and these are the things that must be
//! the run's, not the lane's: the stop flag, the gate a rate limit closes, the run's spend and the ceilings on it, and
//! what the views show of each lane.
//!
//! An engine instance holds an `Option<Arc<Shared>>`: `None` is an ordinary run, and every call site keeps its own code then.
//! The forks of a driver and the lanes of a process file share one: the operator's commands reach them through it, their
//! asks are the run's, and their views are published from it.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
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
    /// a fork the driver dropped without merging it: its work stays on its branch
    Discarded,
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
    /// the run's project folder, when its views, asks and spend are published and kept there (a driver's forks)
    root: OnceLock<PathBuf>,
    /// the forks that are up: their folders, where the operator's commands are sent
    live: Mutex<BTreeMap<String, PathBuf>>,
    /// one fork drains the run's bus at a time
    drain: Mutex<()>,
    /// every call the forks made, summed by (role, agent, model): what the run's report adds to its own usage
    calls: Mutex<Vec<(crate::usage::Role, String, crate::usage::Usage)>>,
    /// commands the forks drained from the run's bus that the run itself has not seen yet
    root_inbox: Mutex<Vec<crate::bus::Command>>,
    /// the run's list of asks is written by one fork at a time (an id is minted from what it holds)
    asks: Mutex<()>,
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
            root: OnceLock::new(),
            live: Mutex::new(BTreeMap::new()),
            drain: Mutex::new(()),
            calls: Mutex::new(Vec::new()),
            root_inbox: Mutex::new(Vec::new()),
            asks: Mutex::new(()),
        })
    }

    /// Services for the forks of the run in `root`: their views are published there, their spend is kept there (so that a
    /// resumed run's ceilings count what the forks spent before it stopped), and the operator's commands are drained from
    /// its bus.
    pub fn for_run(root: &Path, limits: Limits, resume: bool) -> Arc<Shared> {
        let shared = Shared::new(limits);
        let _ = shared.root.set(root.to_path_buf());
        let file = spend_file(root);
        if resume {
            let held = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<(u64, u64)>(&t).ok());
            *shared.spent.lock().unwrap_or_else(|e| e.into_inner()) = held.unwrap_or((0, 0));
        } else {
            let _ = std::fs::remove_file(&file);
        }
        shared
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

impl Shared {
    /// A stop was asked: by the operator, a ceiling, or a failed lane under `fail_fast`; a lane starts nothing new.
    pub fn stop_requested(&self) -> bool {
        self.stop_reason().is_some()
    }

    /// Blocks while the gate of `agent` is closed (a lane met its rate limit); returns when it opens or a stop is asked.
    pub fn wait_rate(&self, agent: &str) {
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

    /// A lane met `agent`'s rate limit, reset at `until` (epoch seconds): no lane launches a session of it before.
    pub fn rate_hit(&self, agent: &str, until: u64) {
        let mut gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let held = gate.entry(agent.to_string()).or_insert(0);
        *held = (*held).max(until);
        self.opened.notify_all();
    }

    /// A call lane `key` made: what it reported, for the run's totals and its report, and its spend as [`Shared::spend`].
    pub fn charge(&self, key: &str, role: crate::usage::Role, agent: &str, usage: &crate::usage::Usage) {
        {
            let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
            match calls.iter_mut().find(|(r, a, u)| *r == role && a == agent && u.model == usage.model) {
                Some((_, _, sum)) => sum.absorb(usage),
                None => calls.push((role, agent.to_string(), usage.clone())),
            }
        }
        self.spend(key, usage.tokens.output_or_zero(), (usage.cost_usd.unwrap_or(0.0).max(0.0) * 1_000_000.0).round() as u64);
    }

    /// Every call the forks made, summed by role, agent and model.
    pub fn calls(&self) -> Vec<(crate::usage::Role, String, crate::usage::Usage)> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// A session of lane `key` spent this much (output tokens, millionths of a dollar): the run's total, the lane's view, and
    /// the stop when a ceiling is broken.
    pub fn spend(&self, key: &str, tokens: u64, cost_micros: u64) {
        let (t, c) = {
            let mut s = self.spent.lock().unwrap_or_else(|e| e.into_inner());
            s.0 += tokens;
            s.1 += cost_micros;
            self.keep_spend(*s);
            (s.0, s.1)
        };
        self.update_view(key, |v| {
            v.tokens += tokens;
            v.cost += cost_micros as f64 / 1_000_000.0;
        });
        if let Some(max) = self.limits.tokens.filter(|m| t >= *m) {
            self.request_stop(&format!("limits.tokens: {t}/{max}"));
        }
        if let Some(max) = self.limits.cost.filter(|m| c as f64 / 1_000_000.0 >= *m) {
            self.request_stop(&format!("limits.cost: ${:.2}/${max:.2}", c as f64 / 1_000_000.0));
        }
    }

    /// The ceiling the run's total spend broke, if one.
    pub fn ceiling_hit(&self) -> Option<String> {
        self.stop_reason().filter(|r| r.starts_with("limits."))
    }

    /// A lane's view, as it is now; published with the others when the run keeps them.
    pub fn view(&self, view: LaneView) {
        self.views.lock().unwrap_or_else(|e| e.into_inner()).insert(view.key.clone(), view);
        self.publish();
    }

    /// Changes lane `key`'s view where it is, when it has one.
    fn update_view(&self, key: &str, change: impl FnOnce(&mut LaneView)) {
        let changed = self.views.lock().unwrap_or_else(|e| e.into_inner()).get_mut(key).map(change).is_some();
        if changed {
            self.publish();
        }
    }

    /// Lane `key` is at step `step`.
    pub fn note_step(&self, key: &str, step: &str) {
        self.update_view(key, |v| v.step = step.to_string());
    }

    /// Lane `key` is waiting out a rate limit, or is not.
    pub fn note_waiting(&self, key: &str, waiting: bool) {
        self.update_view(key, |v| v.waiting = waiting);
    }

    fn publish(&self) {
        if let Some(root) = self.root.get() {
            crate::lanes::publish(root, &self.views());
        }
    }

    fn keep_spend(&self, spent: (u64, u64)) {
        let Some(root) = self.root.get() else { return };
        let file = spend_file(root);
        let temp = file.with_extension("json.tmp");
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if serde_json::to_string(&spent).ok().is_some_and(|t| std::fs::write(&temp, t).is_ok()) {
            let _ = std::fs::rename(&temp, &file);
        }
    }

    /// A fork is up: the operator's commands are sent to its folder from now on.
    pub fn register(&self, key: &str, dir: &Path) {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_string(), dir.to_path_buf());
    }

    /// A fork ended: nothing is sent to it.
    pub fn forget(&self, key: &str) {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }

    /// Drains the run's bus and sends every command to every fork that is up (and keeps it for the run itself): the
    /// operator's `rlt send` reaches all of them, and each applies it at its next boundary as a run does. A stop is also
    /// the run's, for the forks and for what starts later.
    pub fn pump(&self) {
        let Some(root) = self.root.get() else { return };
        let _one = self.drain.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(bus) = crate::bus::Bus::open(root) else { return };
        for cmd in bus.drain() {
            match &cmd {
                crate::bus::Command::Stop { reason } => self.request_stop(reason),
                crate::bus::Command::Pause => self.set_paused(true),
                crate::bus::Command::Resume => self.set_paused(false),
                _ => {}
            }
            let stamp = format!("{:013}", crate::util::now_epoch() * 1000);
            for dir in self.live.lock().unwrap_or_else(|e| e.into_inner()).values() {
                let _ = crate::bus::Bus::open(dir).and_then(|b| b.send(&cmd, &stamp));
            }
            self.root_inbox.lock().unwrap_or_else(|e| e.into_inner()).push(cmd);
        }
    }

    /// The commands the forks drained that the run has yet to see.
    pub fn take_root_inbox(&self) -> Vec<crate::bus::Command> {
        std::mem::take(&mut *self.root_inbox.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Held while an ask is minted and written into the run's list: two forks asking at once get two ids.
    pub fn asks_lock(&self) -> MutexGuard<'_, ()> {
        self.asks.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Where a run keeps what its forks spent.
fn spend_file(root: &Path) -> PathBuf {
    crate::paths::private_dir(root).join("forks").join("spend.json")
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
        c.spend("k", 60, 0);
        assert!(!c.stop_requested());
        c.spend("k", 60, 0);
        assert_eq!(c.ceiling_hit().as_deref(), Some("limits.tokens: 120/100"));
        let d = Shared::new(Limits { tokens: None, cost: Some(0.5) });
        d.spend("k", 1, 600_000);
        assert!(d.ceiling_hit().unwrap().starts_with("limits.cost"));
    }

    /// Charges from eight threads add up exactly.
    #[test]
    fn charges_from_eight_threads_add_up() {
        let s = Shared::new(Limits::default());
        std::thread::scope(|sc| {
            for _ in 0..8 {
                sc.spawn(|| (0..500).for_each(|_| s.spend("k", 2, 1000)));
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
