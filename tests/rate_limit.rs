//! A-18 end to end: the rate-limit wait is visible, wakeable and stoppable, an interrupted run exits
//! 5, and `agg run --resume` continues it with the next session while the interrupted session's work
//! waits on its partial ref.
//!
//! The worker is a FAKE `claude` on PATH that follows a per-invocation PLAN (one line per session):
//! `limit` prints a subscription rate-limit message with a reset time one hour ahead and exits 1;
//! `partial` first leaves half-finished work in the tree, then does the same; `hang` leaves work and
//! sleeps; any other word does that session's work. The plan, the counter and the stub live OUTSIDE
//! the project, so agg's auto-commit never picks them up.

#![cfg(unix)]

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Project {
    _tmp: tempfile::TempDir,
    _tools: tempfile::TempDir,
    dir: PathBuf,
    bin: PathBuf,
    path: String,
}

fn write(p: &Path, content: &str) {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(p, content).unwrap();
}

fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git").args(args).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

/// "resets HH:MM" one hour from now, in local 24h time — the form the stream parser reads.
fn reset_in_an_hour() -> String {
    let at = jiff::Zoned::now().checked_add(jiff::Span::new().hours(1)).unwrap();
    at.strftime("%H:%M").to_string()
}

/// A project on `main` with one committed file, the fake worker, a `finished` judge and `seq` as
/// the sequence. `plan` is the worker's script, one word per session.
fn project(seq: &[&str], plan: &[&str], extra_cfg: &str) -> Project {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    let bin = tools.path().to_path_buf();
    let limit = format!(
        r#"printf '%s\n' '{{"type":"result","subtype":"error","is_error":true,"result":"You have hit your session limit · resets {}","usage":{{"output_tokens":0}},"total_cost_usd":0}}'; exit 1"#,
        reset_in_an_hour()
    );
    write(
        &bin.join("claude"),
        &format!(
            r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "--version" ]; then echo "fake-claude 0.0.0"; exit 0; fi
done
B="{bin}"
n=$(( $(cat "$B/count" 2>/dev/null || echo 0) + 1 )); echo "$n" > "$B/count"
case "$(sed -n "${{n}}p" "$B/plan")" in
  limit) {limit};;
  partial) echo "half done" > wip.txt; echo "edited" >> tracked.txt; {limit};;
  hang) echo "half done" > wip.txt; echo "edited" >> tracked.txt; sleep 30;;
  finish) : > finished;;
  *) echo "session $n" > "work-$n.txt";;
esac
printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{{"output_tokens":1}},"total_cost_usd":0}}'
"#,
            bin = bin.display()
        ),
    );
    chmod_x(&bin.join("claude"));
    write(&bin.join("plan"), &(plan.join("\n") + "\n"));

    let steps: String = seq.iter().map(|s| format!("  {s}: {{}}\n")).collect();
    write(
        &dir.join("agg/agg.yaml"),
        &format!(
            "project: rl\ndefaults: {{ model: fake }}\nsteps:\n{steps}sequence:\n  steps: [{}]\n  done_if: \"finished\"\nsummary: {{ enabled: false }}\n{extra_cfg}",
            seq.join(", ")
        ),
    );
    write(&dir.join("agg/state/STATE.md"), "do work\n");
    let judge = dir.join("agg/judges/finished.sh");
    write(
        &judge,
        "#!/bin/sh\nif [ -f finished ]; then echo '{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"y\"}'; \
         else echo '{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"n\"}'; fi\n",
    );
    chmod_x(&judge);
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git(&dir, args);
    }
    write(&dir.join("tracked.txt"), "base\n");
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "base"]);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    Project { _tmp: tmp, _tools: tools, dir, bin, path }
}

impl Project {
    fn agg(&self) -> Command {
        let mut c = Command::cargo_bin("agg").unwrap();
        c.current_dir(&self.dir).env("PATH", &self.path).env("HOME", self.dir.join(".home"));
        c
    }

    fn spawn_run(&self, extra: &[&str]) -> Child {
        let log = fs::File::create(self.bin.join(format!("run{}.log", extra.len()))).unwrap();
        self.agg()
            .arg("run")
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap()
    }

    fn log(&self, extra: usize) -> String {
        fs::read_to_string(self.bin.join(format!("run{extra}.log"))).unwrap_or_default()
    }

    fn attempts(&self) -> u32 {
        fs::read_to_string(self.bin.join("count")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
    }

    fn status(&self) -> serde_json::Value {
        let out = self.agg().args(["status", "--json"]).output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null)
    }

    fn send(&self, args: &[&str]) {
        let out = self.agg().arg("send").args(args).output().unwrap();
        assert!(out.status.success(), "agg send {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn report(&self) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(agg::paths::report_json(&self.dir)).unwrap()).unwrap()
    }

    fn wait_rows(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(agg::paths::waits_jsonl(&self.dir))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// Poll `cond` for up to `secs`, panicking with `what` (and the run log) if it never holds.
fn until(p: &Project, secs: u64, what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}:\n{}", p.log(0));
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Wait until the loop publishes a `waiting` state while `attempts` sessions have run.
fn until_waiting(p: &Project, attempts: u32) -> serde_json::Value {
    let mut seen = serde_json::Value::Null;
    until(p, 60, "the rate-limit wait", || {
        seen = p.status();
        p.attempts() == attempts && !seen["waiting"].is_null()
    });
    seen["waiting"].clone()
}

fn wait_exit(child: &mut Child, secs: u64) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s.code().unwrap_or(-1);
        }
        assert!(Instant::now() < deadline, "agg run did not exit");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A-18.1 + A-18.2: the wait is visible in `agg status --json` with its reset time; `agg send wake`
/// starts the next session within seconds; a still-limited agent is detected and waited for again;
/// `agg send resume` wakes too. Every wait is on the ledger, start and end.
#[test]
fn a_wait_is_visible_and_wake_or_resume_start_the_next_session() {
    let p = project(&["worker"], &["limit", "limit", "finish"], "ratelimit_min_gap_secs: 1\n");
    let mut child = p.spawn_run(&[]);

    let w = until_waiting(&p, 1);
    let now = agg::util::now_epoch();
    assert_eq!(w["reason"], "rate_limit");
    assert_eq!(w["agent"], "claude");
    let until_at = w["until"].as_u64().unwrap();
    assert!(until_at > now + 3_000 && until_at < now + 3_800, "until ≈ the reset an hour out: {w}");

    let sent = Instant::now();
    p.send(&["wake"]);
    until(&p, 10, "the second attempt", || p.attempts() == 2);
    assert!(sent.elapsed() < Duration::from_secs(5), "wake took {:?}", sent.elapsed());

    // still limited ⇒ detected and waited for again; `resume` ends this one.
    until_waiting(&p, 2);
    p.send(&["resume"]);
    assert_eq!(wait_exit(&mut child, 60), 0, "done_if met after the third attempt:\n{}", p.log(0));

    let rows = p.wait_rows();
    let events: Vec<_> = rows.iter().map(|r| (r["event"].as_str().unwrap(), r["how"].as_str().unwrap_or(""))).collect();
    assert_eq!(events, [("wait", ""), ("wait_end", "woken"), ("wait", ""), ("wait_end", "woken")], "{rows:?}");
    assert!(p.status()["waiting"].is_null(), "a run that finished working is not waiting");
    assert!(rows[0]["partial"].is_null(), "a limited session that changed nothing saves nothing");
    assert_eq!(git(&p.dir, &["for-each-ref", "refs/agg/partial/"]), "");
    let report = p.report();
    assert_eq!(report["outcome"], "done");
    assert!(report.get("waiting").is_none(), "only a run that ENDED waiting reports a wait: {report}");
}

/// A-18.2: `agg stop` during the wait ends the run as stopped (exit 5), and the ledger says how.
#[test]
fn stop_during_the_wait_exits_5() {
    let p = project(&["worker"], &["limit"], "");
    let mut child = p.spawn_run(&[]);
    until_waiting(&p, 1);
    p.send(&["stop"]);
    assert_eq!(wait_exit(&mut child, 20), 5, "{}", p.log(0));
    assert_eq!(p.report()["outcome"], "stopped");
    assert_eq!(p.wait_rows().last().unwrap()["how"], "stopped");
}

/// A-18.3 + A-18.4, the acceptance path: the rate-limited session's half-finished work goes to
/// `refs/agg/partial/<run>/<n>` and the tree is reset; SIGINT during the wait exits 5 with
/// `outcome: stopped`; `agg run --resume` continues the SAME run with session n+1, at the step after
/// the last kept gate, and finishes.
#[test]
fn sigint_during_the_wait_exits_5_and_resume_continues_with_the_next_session() {
    let p = project(&["first", "second"], &["work", "partial", "finish"], "");
    let mut child = p.spawn_run(&[]);
    until_waiting(&p, 2);
    assert_eq!(git(&p.dir, &["status", "--porcelain", "--untracked-files=no"]), "", "the tree is reset:\n{}", p.log(0));
    assert!(!p.dir.join("wip.txt").exists(), "the session's new file left the tree");

    Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    assert_eq!(wait_exit(&mut child, 20), 5, "an interrupted wait is a STOP:\n{}", p.log(0));
    let first = p.report();
    assert_eq!(first["outcome"], "stopped");
    assert_eq!(first["exit_code"], 5);
    assert_eq!(first["waiting"]["reason"], "rate_limit", "the report says it ended waiting: {first}");
    let run_id = first["run_id"].as_str().unwrap().to_string();
    let r = format!("refs/agg/partial/{run_id}/2");
    assert_eq!(git(&p.dir, &["show", &format!("{r}:wip.txt")]), "half done", "the partial work is on {r}");
    assert_eq!(git(&p.dir, &["show", &format!("{r}:tracked.txt")]), "base\nedited");

    let out = p.agg().args(["run", "--resume"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "the resumed run finishes:\n{err}");
    assert!(err.contains("session #3") && err.contains("step `second`"), "session n+1, same step:\n{err}");
    assert!(!err.contains("session #1 ") && !err.contains("step `first`"), "kept work is not redone:\n{err}");
    let second = p.report();
    assert_eq!(second["run_id"], run_id.as_str(), "a resume keeps the run id");
    assert_eq!(second["sessions"]["total"], 3, "…and the session counter");
}

/// A-18.3: an interrupt MID-SESSION saves that session's work the same way, so the run can be
/// resumed (or a fresh one started) from a clean tree.
#[test]
fn sigint_during_a_session_saves_its_work_to_a_partial_ref() {
    let p = project(&["worker"], &["hang", "finish"], "");
    let mut child = p.spawn_run(&[]);
    until(&p, 30, "the worker to start", || p.dir.join("wip.txt").exists());
    Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    assert_eq!(wait_exit(&mut child, 20), 5, "{}", p.log(0));
    let run_id = p.report()["run_id"].as_str().unwrap().to_string();
    assert_eq!(git(&p.dir, &["show", &format!("refs/agg/partial/{run_id}/1:wip.txt")]), "half done");
    assert_eq!(git(&p.dir, &["status", "--porcelain", "--untracked-files=no"]), "", "the tracked edit is reset");
    assert!(!p.dir.join("wip.txt").exists(), "the session's new file left the tree");
    assert_eq!(git(&p.dir, &["branch", "--show-current"]), "main", "HEAD is back on the base branch");
}

/// `--resume` with nothing to continue is refused, naming why.
#[test]
fn resume_without_a_previous_run_is_refused() {
    let p = project(&["worker"], &["finish"], "");
    let out = p.agg().args(["run", "--resume"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no previous run"), "{}", String::from_utf8_lossy(&out.stderr));
}
