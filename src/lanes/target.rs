//! A lane that runs somewhere else than a thread of this process: the four commands that launch it, ask whether it has ended,
//! end it and bring its work back, and the supervisor that runs them.
//!
//! The contract with every target: the lane is a standalone clone of the project (the folder `{lane_dir}`) holding the lane's spec
//! (`rlt/private/lane-spec.json`); `rlt lane --spec` runs it wherever it is started and leaves `rlt/private/lane-result.json` in the
//! clone when it ends; the clone, with its commits, comes back into `{lane_dir}`; the run fetches the lane's branch from it and
//! joins as for a lane of its own. The built-in `ssh` and `container` targets are these four commands written for you; the
//! `command` target is the plug-in point for anything else (a cloud VM, a cluster job).

use super::services::{LaneServices, Shared};
use super::worktree::Lane;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// The four commands of a target, with `{lane_dir}`, `{key}` and `{lane}` (the lane's folder name) to be filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commands {
    /// starts the lane, detached, and returns
    pub launch: String,
    /// exits 0 when the lane has ended, 1 while it runs
    pub poll: String,
    /// ends the lane
    pub kill: Option<String>,
    /// brings the lane's clone back into `{lane_dir}`
    pub fetch: String,
}

/// `text` quoted for a POSIX shell as one word.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `template` with the lane's place filled in.
pub fn expand(template: &str, lane: &Lane) -> String {
    let id = lane.worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    template.replace("{lane_dir}", &quote(&lane.worktree.display().to_string())).replace("{key}", &quote(&lane.key)).replace("{lane}", &id)
}

/// Runs `command` in `cwd` with a shell, the lane's place in its environment (`RLT_LANE_DIR`, `RLT_LANE_KEY`); its exit code and
/// the last of what it said on stderr.
pub fn shell(command: &str, cwd: &Path, lane: &Lane) -> Result<(i32, String), String> {
    let out = Command::new("sh").arg("-c").arg(command).current_dir(cwd)
        .env("RLT_LANE_DIR", &lane.worktree).env("RLT_LANE_KEY", &lane.key).stdin(std::process::Stdio::null()).output()
        .map_err(|e| format!("`{command}`: {e}"))?;
    let said = String::from_utf8_lossy(&out.stderr);
    Ok((out.status.code().unwrap_or(-1), said.trim().lines().last().unwrap_or_default().to_string()))
}

/// What an ssh target needs.
#[derive(Debug, Clone, Default)]
pub struct Ssh {
    pub host: String,
    pub user: Option<String>,
    /// the folder on the host the lanes live under
    pub dir: String,
    /// the `rlt` binary on the host
    pub rlt: String,
    pub options: Vec<String>,
}

impl Ssh {
    fn login(&self) -> String {
        let dest = match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        };
        let options: Vec<String> = self.options.iter().map(|o| quote(o)).collect();
        format!("ssh -o BatchMode=yes {} {}", options.join(" "), quote(&dest))
    }

    /// The commands of an ssh target for `lane`: the clone is sent as a tar stream, `rlt lane` runs there detached (`setsid` where
    /// there is one, its log and pid kept in `rlt/private/` (ignored by git: a lane's session commits everything else)), `lane-result.json` says it ended, and the clone comes back as a tar stream. The
    /// values of `lane.env` (the passed variables) are sourced by the lane's shell and the file removed; they are in the stream,
    /// never in a command line.
    pub fn commands(&self, lane: &Lane) -> Commands {
        let id = lane.worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let remote = format!("{}/{id}", self.dir.trim_end_matches('/'));
        let (login, r) = (self.login(), quote(&remote));
        let start = format!(
            "mkdir -p {r} && tar -C {r} -xf - && cd {r} && ( set -a; [ -f rlt/private/lane.env ] && . ./rlt/private/lane.env; rm -f rlt/private/lane.env; set +a; \
             S=$(command -v setsid || true); $S nohup {rlt} lane --spec rlt/private/lane-spec.json > rlt/private/lane.log 2>&1 < /dev/null & echo $! > rlt/private/lane.pid )",
            rlt = quote(&self.rlt));
        Commands {
            launch: format!("tar -C {{lane_dir}} -cf - . | {login} {}", quote(&start)),
            poll: format!("{login} {}", quote(&format!("test -f {r}/rlt/private/lane-result.json"))),
            kill: Some(format!("{login} {}", quote(&format!("kill $(cat {r}/rlt/private/lane.pid) 2>/dev/null; true")))),
            fetch: format!("{login} {} | tar -C {{lane_dir}} -xf -", quote(&format!("tar -C {r} -cf - ."))),
        }
    }
}

/// The commands of a container target for `lane`: the clone is mounted at `/lane`, `rlt lane` runs there in `image`, the container's
/// state says whether it ended (it is removed when the clone is back; the mount means there is nothing to copy), the variables
/// named in `pass_env` come from this process's environment (docker reads their values itself).
pub fn container_commands(image: &str, network: Option<&str>, pass_env: &[String], lane: &Lane) -> Commands {
    let id = lane.worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = quote(&format!("rlt-lane-{}-{id}", std::process::id()));
    let net = network.map(|n| format!("--network {} ", quote(n))).unwrap_or_default();
    let env: String = pass_env.iter().map(|e| format!("-e {} ", quote(e))).collect();
    Commands {
        launch: format!("docker run -d --name {name} {net}{env}-v {{lane_dir}}:/lane -w /lane {} rlt lane --spec rlt/private/lane-spec.json", quote(image)),
        poll: format!("test \"$(docker inspect -f '{{{{.State.Running}}}}' {name})\" = false"),
        kill: Some(format!("docker kill {name}")),
        fetch: format!("docker rm {name}"),
    }
}

/// How the supervisor ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// the lane ended on its own; its clone is back
    Finished,
    /// the run was stopped: the lane was killed, and what it had is back
    Killed,
}

/// The marker that the lane was launched (a resumed run reattaches to a lane that runs, it does not start a second).
pub const LAUNCHED: &str = "rlt/private/lane-launched";

/// Launches the lane unless it was, then polls until it has ended or the run is stopped, and brings its clone back. A poll that
/// fails five times in a row (a dropped connection that does not come back) is an error; one failure is tried again.
pub fn supervise(commands: &Commands, lane: &Lane, cwd: &Path, services: &Shared, every: Duration) -> Result<Ended, String> {
    let run = |template: &str| shell(&expand(template, lane), cwd, lane);
    let marker = lane.worktree.join(LAUNCHED);
    if !marker.exists() {
        match run(&commands.launch)? {
            (0, _) => {
                let _ = std::fs::write(&marker, crate::util::now_epoch().to_string());
            }
            (code, said) => return Err(format!("launching lane `{}` failed ({code}): {said}", lane.key)),
        }
    }
    let (mut failures, mut killed) = (0, false);
    loop {
        if services.stop_requested() && !killed {
            killed = true;
            if let Some(kill) = &commands.kill {
                let _ = run(kill);
            }
        }
        match run(&commands.poll)? {
            (0, _) => break,
            (1, _) => failures = 0,
            (code, said) => {
                failures += 1;
                if failures >= 5 {
                    return Err(format!("polling lane `{}` failed {failures} times ({code}): {said}", lane.key));
                }
            }
        }
        if killed {
            break;
        }
        std::thread::sleep(every);
    }
    match run(&commands.fetch)? {
        (0, _) => Ok(if killed { Ended::Killed } else { Ended::Finished }),
        (code, said) => Err(format!("fetching lane `{}` back failed ({code}): {said}", lane.key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanes::services::Limits;

    fn lane(dir: &Path) -> Lane {
        Lane { key: "U 1".into(), each: "units".into(), worktree: dir.join("units-U-1"), dir: dir.join("units-U-1"), branch: "b".into(), project: "p".into() }
    }

    fn commands(launch: &str, poll: &str, kill: Option<&str>, fetch: &str) -> Commands {
        Commands { launch: launch.into(), poll: poll.into(), kill: kill.map(String::from), fetch: fetch.into() }
    }

    /// Placeholders are filled in and quoted; the environment names the lane.
    #[test]
    fn a_template_is_filled_in_with_the_lane_and_quoted() {
        let d = tempfile::tempdir().unwrap();
        let l = lane(d.path());
        assert_eq!(expand("go {lane_dir} {key} {lane}", &l), format!("go '{}' 'U 1' units-U-1", l.worktree.display()));
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(shell("echo $RLT_LANE_KEY >&2; exit 3", d.path(), &l).unwrap(), (3, "U 1".to_string()));
    }

    /// The ssh commands send the clone as a stream, start the lane detached, poll its result file and bring the clone back; the
    /// passed variables are in the stream, not in any command; the container commands mount the clone and read the container's state.
    #[test]
    fn the_built_in_targets_write_their_commands() {
        let d = tempfile::tempdir().unwrap();
        let l = lane(d.path());
        let ssh = Ssh { host: "build-box".into(), user: Some("deploy".into()), dir: "/srv/rlt/".into(), rlt: "~/.local/bin/rlt".into(),
                        options: vec!["-o".into(), "ServerAliveInterval=30".into()] };
        let c = ssh.commands(&l);
        assert!(c.launch.starts_with("tar -C {lane_dir} -cf - . | ssh -o BatchMode=yes '-o' 'ServerAliveInterval=30' 'deploy@build-box' "), "{}", c.launch);
        assert!(c.launch.contains("rlt lane --spec rlt/private/lane-spec.json") && c.launch.contains("/srv/rlt/units-U-1") && c.launch.contains("rlt/private/lane.pid"));
        assert!(c.launch.contains("rm -f rlt/private/lane.env") && !c.launch.contains("ANTHROPIC"), "the values are not in a command");
        assert!(c.poll.contains("test -f '/srv/rlt/units-U-1'/rlt/private/lane-result.json"), "{}", c.poll);
        assert!(c.fetch.ends_with("| tar -C {lane_dir} -xf -") && c.kill.unwrap().contains("rlt/private/lane.pid"));
        let k = container_commands("rlt-claude:0.5.0", Some("none"), &["ANTHROPIC_API_KEY".into()], &l);
        assert!(k.launch.contains("-e 'ANTHROPIC_API_KEY' ") && k.launch.contains("--network 'none'") && k.launch.contains("-v {lane_dir}:/lane"), "{}", k.launch);
        assert!(k.poll.contains("docker inspect") && k.kill.unwrap().starts_with("docker kill 'rlt-lane-") && k.fetch.starts_with("docker rm"));
    }

    /// Launch once, poll until it has ended, fetch: the marker keeps a second supervisor from launching again.
    #[test]
    fn a_lane_is_launched_once_polled_to_its_end_and_fetched() {
        let d = tempfile::tempdir().unwrap();
        let l = lane(d.path());
        std::fs::create_dir_all(l.worktree.join("rlt/private")).unwrap();
        let log = d.path().join("log");
        let note = |what: &str| format!("echo {what} >> {}", quote(&log.display().to_string()));
        // the lane "ends" at the third poll
        let poll = format!("n=$(cat {c} 2>/dev/null || echo 0); echo $((n+1)) > {c}; [ $n -ge 2 ]", c = quote(&d.path().join("count").display().to_string()));
        let c = commands(&note("launch"), &poll, None, &note("fetch"));
        let services = Shared::new(Limits::default());
        assert_eq!(supervise(&c, &l, d.path(), &services, Duration::from_millis(20)).unwrap(), Ended::Finished);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "launch\nfetch\n");
        assert_eq!(supervise(&c, &l, d.path(), &services, Duration::from_millis(20)).unwrap(), Ended::Finished);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "launch\nfetch\nfetch\n", "launched once");
    }

    /// A stop kills the lane and still fetches what it had; a launch that fails and a poll that keeps failing are errors.
    #[test]
    fn a_stop_kills_and_fetches_and_failures_are_errors() {
        let d = tempfile::tempdir().unwrap();
        let l = lane(d.path());
        std::fs::create_dir_all(l.worktree.join("rlt/private")).unwrap();
        let log = d.path().join("log");
        let note = |what: &str| format!("echo {what} >> {}", quote(&log.display().to_string()));
        let services = Shared::new(Limits::default());
        services.request_stop("operator");
        let c = commands("true", "exit 1", Some(&note("kill")), &note("fetch"));
        assert_eq!(supervise(&c, &l, d.path(), &services, Duration::from_millis(10)).unwrap(), Ended::Killed);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "kill\nfetch\n");
        let l2 = Lane { worktree: d.path().join("units-b"), dir: d.path().join("units-b"), ..lane(d.path()) };
        std::fs::create_dir_all(l2.worktree.join("rlt/private")).unwrap();
        let live = Shared::new(Limits::default());
        let bad_launch = commands("echo nope >&2; exit 4", "true", None, "true");
        assert!(supervise(&bad_launch, &l2, d.path(), &live, Duration::from_millis(10)).unwrap_err().contains("launching lane `U 1` failed (4): nope"));
        let bad_poll = commands("true", "echo gone >&2; exit 255", None, "true");
        let err = supervise(&bad_poll, &l2, d.path(), &live, Duration::from_millis(5)).unwrap_err();
        assert!(err.contains("failed 5 times (255): gone"), "{err}");
    }
}
