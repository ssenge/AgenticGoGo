//! The run's side while lanes work: the operator's commands forwarded to the lanes (each lane has a bus of its own), their
//! asks mirrored into the run's ledger as `<key>:<id>` with the answers sent back, and their views kept and published.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::services::{LaneServices, LaneView, Shared};
use super::worktree::Lane;

/// Forwards what the operator sends the run to its lanes, mirrors the lanes' asks into the run's, and keeps the views.
pub struct Forwarder<'a> {
    project: &'a Path,
    lanes: Vec<(usize, &'a Lane)>,
    services: &'a Arc<Shared>,
    held: Mutex<Vec<(usize, crate::bus::Command)>>,
    mirrored: Mutex<Vec<(usize, String, String)>>,
    stop: Mutex<Option<String>>,
}

impl<'a> Forwarder<'a> {
    pub fn new(project: &'a Path, lanes: &'a [Lane], todo: &[usize], services: &'a Arc<Shared>) -> Forwarder<'a> {
        let all = todo.iter().map(|i| (*i, &lanes[*i])).collect();
        for i in todo {
            services.view(LaneView::queued(&lanes[*i].key));
        }
        Forwarder { project, lanes: all, services, held: Mutex::default(), mirrored: Mutex::default(), stop: Mutex::default() }
    }

    fn lane(&self, index: usize) -> Option<&Lane> {
        self.lanes.iter().find(|(i, _)| *i == index).map(|(_, l)| *l)
    }

    /// One pass: the run's bus drained and sent on to `running` lanes, their asks mirrored, their views updated.
    pub fn tick(&self, running: &[usize]) {
        if let Ok(bus) = crate::bus::Bus::open(self.project) {
            for cmd in bus.drain() {
                match &cmd {
                    crate::bus::Command::Stop { reason } => {
                        self.services.request_stop(reason);
                        *self.stop.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
                    }
                    crate::bus::Command::Pause => self.services.set_paused(true),
                    crate::bus::Command::Resume => self.services.set_paused(false),
                    _ => {}
                }
                let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
                held.extend(running.iter().map(|r| (*r, cmd.clone())));
            }
        }
        self.send_held(running);
        for r in running {
            if let Some(lane) = self.lane(*r) {
                self.mirror_asks(*r, lane);
                self.update_view(lane);
            }
        }
        crate::lanes::publish(self.project, &self.services.views());
    }

    /// Sends the held commands to the lanes that are up (a lane that has not started would purge its bus as it boots).
    fn send_held(&self, running: &[usize]) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        held.retain(|(i, cmd)| {
            let Some(lane) = self.lane(*i) else { return false };
            if !running.contains(i) {
                return false;
            }
            if crate::os::detach::live_pid(&lane.dir).is_none() {
                return true;
            }
            let sent = crate::bus::Bus::open(&lane.dir).and_then(|b| b.send(cmd, &format!("{:013}", crate::util::now_epoch() * 1000)));
            sent.is_err()
        });
    }

    /// Asks a lane opened are opened in the run's ledger as `<key>:<id>`; an answer given there goes to the lane's.
    fn mirror_asks(&self, index: usize, lane: &Lane) {
        let mut mirrored = self.mirrored.lock().unwrap_or_else(|e| e.into_inner());
        for ask in crate::core::asks::open(&lane.dir) {
            if mirrored.iter().any(|(i, id, _)| *i == index && *id == ask.id) {
                continue;
            }
            let id = format!("{}:{}", lane.key, ask.id);
            let row = crate::core::asks::Row::Open { id: id.clone(), case: ask.case, question: ask.question.clone(), options: ask.options.clone(),
                origin: ask.origin, session: ask.session, step: ask.step.clone(), ts: ask.opened_at_epoch };
            if crate::core::asks::append(self.project, &row).is_ok() {
                mirrored.push((index, ask.id.clone(), id));
            }
        }
        for (i, lane_id, run_id) in mirrored.iter() {
            if *i != index {
                continue;
            }
            let answered = crate::core::asks::get(self.project, run_id).and_then(|a| a.answer.map(|x| (x, a.by.unwrap_or_default())));
            let open = crate::core::asks::get(&lane.dir, lane_id).is_some_and(|a| a.is_open());
            if let (Some((answer, by)), true) = (answered, open) {
                let _ = crate::core::asks::answer(&lane.dir, lane_id, &answer, &by, crate::util::now_epoch());
            }
        }
    }

    /// The lane's view from the state it publishes.
    fn update_view(&self, lane: &Lane) {
        let Some(dash) = crate::state::DashboardState::read(&lane.dir) else { return };
        let mut view = self.services.views().into_iter().find(|v| v.key == lane.key).unwrap_or_else(|| LaneView::queued(&lane.key));
        (view.step, view.tokens, view.cost, view.waiting) = (dash.step.clone(), dash.tokens_spent, dash.cost_spent, dash.waiting.is_some());
        self.services.view(view);
    }

    /// The lanes are done: their views are no longer published, and asks mirrored for the last time are no longer waited on.
    pub fn finished(&self) {
        let _ = std::fs::remove_file(crate::lanes::views_file(self.project));
        let mirrored = self.mirrored.lock().unwrap_or_else(|e| e.into_inner());
        for (_, _, run_id) in mirrored.iter() {
            if crate::core::asks::get(self.project, run_id).is_some_and(|a| a.is_open()) {
                let row = crate::core::asks::Row::Withdrawn { id: run_id.clone(), reason: "its lane ended".into(), ts: crate::util::now_epoch() };
                let _ = crate::core::asks::append(self.project, &row);
            }
        }
    }

    /// A stop the lanes took is sent to the run's own bus again, so the run ends the way a stop ends it.
    pub fn requeue_stop(&self, why: &str) -> bool {
        if self.stop.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
            return false;
        }
        crate::bus::Bus::open(self.project)
            .and_then(|bus| bus.send(&crate::bus::Command::Stop { reason: why.to_string() }, &format!("{:013}", crate::util::now_epoch() * 1000)))
            .is_ok()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::services::Limits;
    use crate::core::asks::{self, AskCase, Origin, Row};

    fn open(dir: &Path, id: &str) {
        let row = Row::Open { id: id.into(), case: AskCase::Input, question: "which one?".into(), options: None, origin: Origin::Worker,
                              session: 1, step: Some("make".into()), ts: 100 };
        asks::append(dir, &row).unwrap();
    }

    /// A lane's ask shows in the run's ledger as `<key>:<id>`, once; an answer given there reaches the lane's ask; asks of
    /// lanes that are done are withdrawn from the run's ledger.
    #[test]
    fn a_lanes_ask_is_mirrored_into_the_runs_and_its_answer_goes_back() {
        let (run, lane_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let lane = Lane { key: "U1".into(), each: "units".into(), worktree: lane_dir.path().into(), dir: lane_dir.path().into(),
                          branch: "b".into(), project: "p".into() };
        let services = Shared::new(Limits::default());
        let lanes = [lane];
        let fwd = Forwarder::new(run.path(), &lanes, &[0], &services);
        open(lane_dir.path(), "ask-1");
        fwd.tick(&[0]);
        fwd.tick(&[0]);
        let mine = asks::open(run.path());
        assert_eq!(mine.len(), 1, "mirrored once");
        assert_eq!((mine[0].id.as_str(), mine[0].question.as_str()), ("U1:ask-1", "which one?"));
        asks::answer(run.path(), "U1:ask-1", "the second", "human", 200).unwrap();
        fwd.tick(&[0]);
        let theirs = asks::get(lane_dir.path(), "ask-1").unwrap();
        assert_eq!((theirs.answer.as_deref(), theirs.by.as_deref()), (Some("the second"), Some("human")));
        open(lane_dir.path(), "ask-2");
        fwd.tick(&[0]);
        fwd.finished();
        assert!(asks::open(run.path()).is_empty(), "an ask of a lane that ended is no one's to answer: {:?}", asks::open(run.path()));
    }

    /// The operator's stop and pause on the run's bus: the services stop, the pause holds the lanes back, and the commands
    /// are sent on to a lane once it is up (not before: a lane purges its bus as it boots).
    #[test]
    fn the_operators_commands_reach_the_lanes_once_they_are_up() {
        let (run, lane_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let lane = Lane { key: "U1".into(), each: "units".into(), worktree: lane_dir.path().into(), dir: lane_dir.path().into(),
                          branch: "b".into(), project: "p".into() };
        let services = Shared::new(Limits::default());
        let lanes = [lane];
        let fwd = Forwarder::new(run.path(), &lanes, &[0], &services);
        let bus = crate::bus::Bus::open(run.path()).unwrap();
        bus.send(&crate::bus::Command::Pause, "0000000000001").unwrap();
        fwd.tick(&[0]);
        assert!(services.paused() && !services.stop_requested());
        let lane_bus = crate::bus::Bus::open(lane_dir.path()).unwrap();
        assert!(lane_bus.drain().is_empty(), "the lane is not up yet: nothing sent");
        // the lane boots: its guard names a live process
        std::fs::create_dir_all(crate::paths::private_dir(lane_dir.path())).unwrap();
        std::fs::write(crate::paths::private_dir(lane_dir.path()).join("run.pid"), std::process::id().to_string()).unwrap();
        bus.send(&crate::bus::Command::Stop { reason: "enough".into() }, "0000000000002").unwrap();
        fwd.tick(&[0]);
        assert_eq!(services.stop_reason().as_deref(), Some("enough"));
        let got: Vec<String> = lane_bus.drain().iter().map(|c| format!("{c:?}")).collect();
        assert!(got.iter().any(|c| c.starts_with("Pause")) && got.iter().any(|c| c.contains("enough")), "{got:?}");
        assert!(fwd.requeue_stop("enough"));
        assert!(!bus.drain().is_empty(), "the stop is on the run's own bus again");
    }
}
