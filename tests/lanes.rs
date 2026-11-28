//! Parallel lanes through the real binary: `examples/parallel` copied into a temp project, a worker that sleeps, lanes in
//! worktrees of their own, joined in item order.

mod common;

use common::Bounded;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(["-C", &dir.display().to_string()]).args(args).output_bounded().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `examples/parallel` as a project in a temp folder, a git repo, with `edit` applied to its files first: the process
/// file and the worker script are patched with `(from, to)` pairs.
struct Fixture {
    _tmp: tempfile::TempDir,
    process: PathBuf,
    project: PathBuf,
}

fn fixture(process: &[(&str, &str)], worker: &[(&str, &str)], plan: Option<&str>) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/parallel");
    let home = tmp.path().join("home");
    copy(&src, &home);
    let project = tmp.path().join("demo");
    fs::rename(home.join("projects/demo"), &project).unwrap();
    let patch = |file: &Path, pairs: &[(&str, &str)]| {
        let mut text = fs::read_to_string(file).unwrap();
        for (from, to) in pairs {
            assert!(text.contains(from), "`{from}` is not in {}", file.display());
            text = text.replace(from, to);
        }
        fs::write(file, text).unwrap();
    };
    patch(&home.join("process.yaml"), process);
    patch(&project.join("agents/worker.sh"), worker);
    if let Some(p) = plan {
        fs::write(project.join("plan.yaml"), p).unwrap();
    }
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.email", "t@t"], &["config", "user.name", "t"],
                 &["config", "commit.gpgsign", "false"], &["add", "-A"], &["commit", "-qm", "start"]] {
        git(&project, args);
    }
    Fixture { process: home.join("process.yaml"), project, _tmp: tmp }
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy(&src, &dst);
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

/// `rlt process <process> <project> <args…>`, bounded; the exit code and what it said.
fn rlt(f: &Fixture, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rlt"))
        .arg("process")
        .arg(&f.process)
        .arg(&f.project)
        .args(args)
        .env("HOME", f.project.join(".home"))
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output_for(300)
        .unwrap();
    (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

/// The `worked <start> to <end>` window of each unit's file, by unit.
fn windows(project: &Path) -> Vec<(String, u64, u64)> {
    let mut out: Vec<(String, u64, u64)> = fs::read_dir(project.join("out")).unwrap().flatten().filter_map(|e| {
        let text = fs::read_to_string(e.path()).ok()?;
        let w: Vec<&str> = text.split_whitespace().collect();
        Some((w.get(1)?.trim_end_matches(',').to_string(), w.get(3)?.parse().ok()?, w.get(5)?.parse().ok()?))
    }).collect();
    out.sort();
    out
}

fn peak(windows: &[(String, u64, u64)]) -> usize {
    windows.iter().map(|(_, s, _)| windows.iter().filter(|(_, a, b)| a <= s && s < b).count()).max().unwrap_or(0)
}

const FOUR: &str = "units:\n  - { id: a }\n  - { id: b }\n  - { id: c }\n  - { id: d }\n";

/// Four units at four lanes: they overlap, join in item order with one merge commit each, the run's history has each
/// lane's lines, and the worktrees and branches are gone.
#[test]
fn lanes_run_at_once_and_join_in_item_order() {
    // long enough that lanes whose start-up is slow on a loaded machine still overlap
    let f = fixture(&[], &[("sleep 2", "sleep 8")], Some(FOUR));
    let (code, said) = rlt(&f, &[]);
    assert_eq!(code, 0, "{said}");
    let w = windows(&f.project);
    assert_eq!(w.len(), 4, "{w:?}");
    assert!(peak(&w) >= 2, "the lanes overlapped: {w:?}");
    let joins = git(&f.project, &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]);
    assert_eq!(joins, "rlt: join units a\nrlt: join units b\nrlt: join units c\nrlt: join units d");
    let history = fs::read_to_string(f.project.join("reports/steps.jsonl")).unwrap();
    assert!(["make-a", "make-b", "make-c", "make-d"].iter().all(|id| history.contains(id)), "{history}");
    assert!(["units:a", "units:b", "units:c", "units:d"].iter().all(|id| history.contains(id)), "{history}");
    assert!(!f.project.join("rlt/private/lanes/units-a").exists(), "a joined lane's worktree goes");
    assert!(git(&f.project, &["branch", "--list", "rlt/*/lane-*"]).is_empty(), "and its branch");
    assert!(fs::read_to_string(f.project.join("summary.txt")).unwrap().contains("unit d"));
}

/// `max: 1` runs the lanes one after another; `max: 2` never has three at once.
#[test]
fn max_bounds_how_many_lanes_run_at_once() {
    let one = fixture(&[("max: 4", "max: 1")], &[("sleep 2", "sleep 2")], Some("units:\n  - { id: a }\n  - { id: b }\n"));
    assert_eq!(rlt(&one, &[]).0, 0);
    assert_eq!(peak(&windows(&one.project)), 1, "{:?}", windows(&one.project));
    let two = fixture(&[("max: 4", "max: 2")], &[("sleep 2", "sleep 2")], Some("units:\n  - { id: a }\n  - { id: b }\n  - { id: c }\n  - { id: d }\n  - { id: e }\n"));
    assert_eq!(rlt(&two, &[]).0, 0);
    assert!(peak(&windows(&two.project)) <= 2, "{:?}", windows(&two.project));
}

/// Two lanes that change the same file conflict at the join: the second is named with the file, the first stays joined,
/// and the second's worktree stays for the hand merge.
#[test]
fn a_conflict_fails_the_each_and_names_the_lane_and_the_files() {
    let f = fixture(&[], &[("sleep 2", "sleep 1; echo \"$item\" > shared.txt; git add shared.txt")], Some("units:\n  - { id: a }\n  - { id: b }\n"));
    let (code, said) = rlt(&f, &[]);
    assert_eq!(code, 3, "{said}");
    assert!(said.contains("lane `b` conflicts with the lanes joined before it in shared.txt"), "{said}");
    assert!(said.contains("rlt/") && said.contains("lane-units-b"), "the branch to merge by hand is named: {said}");
    assert_eq!(git(&f.project, &["log", "--format=%s", "--grep=^rlt: join"]), "rlt: join units a");
    assert!(f.project.join("rlt/private/lanes/units-b").exists(), "the failed lane's worktree stays");
    assert_eq!(fs::read_to_string(f.project.join("shared.txt")).unwrap().trim(), "a");
}

/// What `--check` refuses: a checkpoint inside a parallel each, `max: 0`.
#[test]
fn a_parallel_each_refuses_a_checkpoint_and_no_room() {
    let with_cp = fixture(
        &[("      checks: [verifiers/made.sh]    # runs in the lane, with RLT_ITEM set\n",
           "      checks: [verifiers/made.sh]\n    - { id: cp-unit, name: Approve, kind: checkpoint, doc: summary.txt, question: \"ok?\" }\n")],
        &[], None);
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).args(["process"]).arg(&with_cp.process).arg("--check").output_bounded().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && said.contains("a checkpoint inside a parallel `each`"), "{said}");
    let none = fixture(&[("max: 4", "max: 0")], &[], None);
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).args(["process"]).arg(&none.process).arg("--check").output_bounded().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && said.contains("`parallel.max` must be at least 1"), "{said}");
}

/// Item keys that become one name are refused at the start, naming both.
#[test]
fn item_keys_that_become_one_name_are_refused() {
    let f = fixture(&[], &[], Some("units:\n  - { id: \"a b\" }\n  - { id: \"a-b\" }\n"));
    let (code, said) = rlt(&f, &[]);
    assert_ne!(code, 0);
    assert!(said.contains("`a b` and `a-b` both become `a-b`"), "{said}");
}

/// A worker that fails the unit `c` unless `LANE_OK` is set: a cause a human removes between two runs.
const FAILS_C: (&str, &str) = ("start=$(date +%s)", "[ \"$item\" = c ] && [ -z \"$LANE_OK\" ] && { echo \"c needs LANE_OK\"; exit 1; }\nstart=$(date +%s)");

/// A lane that fails keeps its worktree; the lanes before it are joined, the one after it is not (the join stays in item
/// order). After the cause is removed `--resume` runs only what is left: the joined lanes don't run again, and the end
/// state is the one an uninterrupted run gives.
#[test]
fn a_failed_lane_resumes_and_the_joined_lanes_do_not_run_again() {
    let f = fixture(&[], &[FAILS_C, ("sleep 2", "sleep 1")], Some("units:\n  - { id: a }\n  - { id: b }\n  - { id: c }\n  - { id: d }\n"));
    let (code, said) = rlt(&f, &[]);
    assert_eq!(code, 3, "{said}");
    assert!(said.contains("lane `c` failed"), "{said}");
    let joined = git(&f.project, &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]);
    assert_eq!(joined, "rlt: join units a\nrlt: join units b", "d finished but waits for c");
    assert!(f.project.join("rlt/private/lanes/units-c").exists() && f.project.join("rlt/private/lanes/units-d").exists());
    let before: Vec<String> = ["a", "b"].iter().map(|u| fs::read_to_string(f.project.join(format!("out/{u}.txt"))).unwrap()).collect();
    let out = Command::new(env!("CARGO_BIN_EXE_rlt"))
        .arg("process").arg(&f.process).arg(&f.project).arg("--resume")
        .env("HOME", f.project.join(".home")).env("LANE_OK", "1")
        .output_for(300).unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{said}");
    let after: Vec<String> = ["a", "b"].iter().map(|u| fs::read_to_string(f.project.join(format!("out/{u}.txt"))).unwrap()).collect();
    assert_eq!(before, after, "the joined lanes did not run again");
    let joins = git(&f.project, &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]);
    assert_eq!(joins, "rlt: join units a\nrlt: join units b\nrlt: join units c\nrlt: join units d");
    assert_eq!(windows(&f.project).len(), 4);
    assert!(git(&f.project, &["worktree", "list"]).lines().count() == 1, "no lane's worktree is left");
}

/// `fail_fast`: a failed lane stops the running ones and starts no more; the units after it never run.
#[test]
fn fail_fast_stops_the_others_and_starts_no_more() {
    let f = fixture(&[("max: 4", "max: 1, fail_fast: true")], &[FAILS_C, ("sleep 2", "sleep 1")],
                    Some("units:\n  - { id: a }\n  - { id: c }\n  - { id: d }\n"));
    let (code, said) = rlt(&f, &[]);
    assert_eq!(code, 3, "{said}");
    assert!(!f.project.join("out/d.txt").exists() && !f.project.join("rlt/private/lanes/units-d").exists(), "d was never started");
    assert_eq!(git(&f.project, &["log", "--format=%s", "--grep=^rlt: join"]), "rlt: join units a");
}

/// The operator's `rlt stop` reaches every lane: the run ends stopped (exit 5), the lanes' sessions end at their boundary,
/// and what is left can be resumed.
#[test]
fn stop_reaches_the_lanes_and_the_run_ends_stopped() {
    use std::os::unix::process::CommandExt;
    let f = fixture(&[], &[("sleep 2", "sleep 25")], Some("units:\n  - { id: a }\n  - { id: b }\n"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_rlt"))
        .arg("process").arg(&f.process).arg(&f.project)
        .env("HOME", f.project.join(".home"))
        .process_group(0).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    // both lanes are up when their worktrees hold a published state
    let up = std::time::Instant::now();
    while !["a", "b"].iter().all(|u| f.project.join(format!("rlt/private/lanes/units-{u}/rlt/private/state.json")).exists()) {
        assert!(up.elapsed().as_secs() < 120, "the lanes did not start");
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let said = Command::new(env!("CARGO_BIN_EXE_rlt")).args(["stop"]).current_dir(&f.project).env("HOME", f.project.join(".home"))
        .output_bounded().unwrap();
    assert!(said.status.success(), "{}{}", String::from_utf8_lossy(&said.stdout), String::from_utf8_lossy(&said.stderr));
    let end = std::time::Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if end.elapsed().as_secs() > 150 {
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(child.id() as i32), nix::sys::signal::Signal::SIGKILL);
            panic!("the run did not end after the stop");
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    };
    assert_eq!(code, Some(5), "stopped");
    assert!(git(&f.project, &["log", "--format=%s", "--grep=^rlt: join"]).is_empty(), "a stopped lane is not joined");
}

/// `examples/parallel` plays through its scenario without tokens: eight lanes, joined in item order, then the script step.
#[test]
fn the_parallel_example_plays_through_its_scenario() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/parallel");
    let out = Command::new(env!("CARGO_BIN_EXE_rlt"))
        .arg("process").arg(root.join("process.yaml"))
        .arg("--scenario").arg(root.join("scenario.yaml"))
        .arg("--projects").arg(root.join("projects"))
        .output_for(600).unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && said.contains("PASS scenario"), "{said}");
}

/// A fake `ssh` first on PATH: it runs the command it is given (its last argument) with a shell, here, so a "remote" folder is
/// a folder of this machine and the tar streams of the ssh target run for real.
fn fake_ssh(dir: &Path) -> String {
    let bin = dir.join("fakebin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("ssh"), "#!/bin/sh\nfor last in \"$@\"; do :; done\nexec sh -c \"$last\"\n").unwrap();
    let mut p = fs::metadata(bin.join("ssh")).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
    fs::set_permissions(bin.join("ssh"), p).unwrap();
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default())
}

fn ssh_fixture(remote: &Path, worker: &[(&str, &str)], plan: &str) -> Fixture {
    let target = format!("parallel:\n      max: 3\n      target: {{ kind: ssh, host: fake-host, user: deploy, dir: \"{}\", rlt: \"{}\", pass_env: [LANE_SECRET] }}\n    #",
                         remote.display(), env!("CARGO_BIN_EXE_rlt"));
    fixture(&[("parallel: { max: 4 }", &target)], worker, Some(plan))
}

/// The ssh target: each lane is a clone sent over a (fake) ssh, run there by `rlt lane`, polled and fetched back; the variables
/// it passes reach the lane in a file, not a command line; the lanes join in item order as for lanes of this process.
#[test]
fn an_ssh_lane_runs_remotely_and_joins_like_a_local_one() {
    let remote = tempfile::tempdir().unwrap();
    let f = ssh_fixture(remote.path(), &[("sleep 2", "sleep 1"), ("git add out", "echo \"secret=$LANE_SECRET\" >> \"out/$item.txt\"; git add out")], "units:\n  - { id: a }\n  - { id: b }\n");
    let path = fake_ssh(remote.path());
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).arg("process").arg(&f.process).arg(&f.project)
        .env("HOME", f.project.join(".home")).env("PATH", path).env("LANE_SECRET", "s3cr3t-value").env("RLT_LANE_POLL_MS", "300")
        .output_for(600).unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{said}");
    let a = fs::read_to_string(f.project.join("out/a.txt")).unwrap();
    assert!(a.contains("secret=s3cr3t-value"), "the passed variable reached the lane: {a}");
    assert_eq!(git(&f.project, &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]), "rlt: join units a\nrlt: join units b");
    assert!(!said.contains("s3cr3t-value"), "the value is in no log line");
    for lane in ["units-a", "units-b"] {
        assert!(remote.path().join(lane).join("rlt/private/lane-result.json").is_file(), "the lane ran in the remote folder");
        assert!(!remote.path().join(lane).join("rlt/private/lane.env").exists(), "the env file was removed there");
        assert!(!f.project.join("rlt/private/lanes").join(lane).exists(), "a joined lane's clone goes");
    }
}

/// A remote lane that fails comes back with its error, keeps its clone, and fails the each with the lane named.
#[test]
fn a_failing_ssh_lane_is_reported_and_keeps_its_clone() {
    let remote = tempfile::tempdir().unwrap();
    let f = ssh_fixture(remote.path(), &[FAILS_C, ("sleep 2", "sleep 1")], "units:\n  - { id: a }\n  - { id: c }\n");
    let path = fake_ssh(remote.path());
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).arg("process").arg(&f.process).arg(&f.project)
        .env("HOME", f.project.join(".home")).env("PATH", path).env("LANE_SECRET", "x").env("RLT_LANE_POLL_MS", "300")
        .output_for(600).unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(3), "{said}");
    assert!(said.contains("lane `c` failed"), "{said}");
    assert_eq!(git(&f.project, &["log", "--format=%s", "--grep=^rlt: join"]), "rlt: join units a");
    assert!(f.project.join("rlt/private/lanes/units-c/rlt/private/lane-result.json").is_file(), "its clone and its result are kept");
}

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    let mut p = fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut p, 0o755);
    fs::set_permissions(path, p).unwrap();
}

/// The `command` target: the four commands are the user's own (here: run `rlt lane` in the clone, test for its result file,
/// nothing to fetch), and the lanes join as any lane does.
#[test]
fn a_command_target_runs_the_users_own_launcher() {
    let target = "parallel:\n      max: 3\n      target:\n        kind: command\n        launch: \"cd {lane_dir} && (nohup $RLT_BIN lane --spec rlt/private/lane-spec.json > rlt/private/lane.log 2>&1 &)\"\n        poll: \"test -f {lane_dir}/rlt/private/lane-result.json\"\n        fetch: \"true\"\n    #";
    let f = fixture(&[("parallel: { max: 4 }", target)], &[("sleep 2", "sleep 1")], Some("units:\n  - { id: a }\n  - { id: b }\n"));
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).arg("process").arg(&f.process).arg(&f.project)
        .env("HOME", f.project.join(".home")).env("RLT_BIN", env!("CARGO_BIN_EXE_rlt")).env("RLT_LANE_POLL_MS", "300")
        .output_for(600).unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert_eq!(git(&f.project, &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]), "rlt: join units a\nrlt: join units b");
    assert_eq!(windows(&f.project).len(), 2);
}

/// The container target with a fake `docker`: the clone is mounted, `rlt lane` runs "in the image", the container's state says
/// when it ended, the variables it passes are named (not valued) on the command.
#[test]
fn a_container_lane_runs_in_the_image_and_joins() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("fakebin");
    let state = tmp.path().join("containers");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&state).unwrap();
    executable(&bin.join("docker"), r#"#!/bin/sh
echo "docker $*" >> "$STATE/calls"
cmd=$1; shift
case $cmd in
  run) name=; mount=
       while [ $# -gt 0 ]; do case $1 in -d) shift;; --name) name=$2; shift 2;; --network) shift 2;; -e) shift 2;; -v) mount=${2%%:*}; shift 2;; -w) shift 2;; *) break;; esac; done
       shift; shift   # the image, then `rlt`
       (cd "$mount" && nohup "$RLT_BIN" "$@" > rlt/private/lane.log 2>&1 & echo $! > "$STATE/$name.pid"); echo "$name" ;;
  inspect) pid=$(cat "$STATE/$3.pid"); if kill -0 "$pid" 2>/dev/null; then echo true; else echo false; fi ;;
  rm) rm -f "$STATE/$1.pid" ;;
  kill) kill "$(cat "$STATE/$1.pid")" ;;
esac
"#);
    let target = "parallel:\n      max: 2\n      target: { kind: container, image: fake-image, network: none, pass_env: [LANE_SECRET] }\n    #";
    let f = fixture(&[("parallel: { max: 4 }", target)], &[("sleep 2", "sleep 1")], Some("units:\n  - { id: a }\n  - { id: b }\n"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).arg("process").arg(&f.process).arg(&f.project)
        .env("HOME", f.project.join(".home")).env("PATH", path).env("STATE", &state).env("RLT_BIN", env!("CARGO_BIN_EXE_rlt"))
        .env("LANE_SECRET", "s3cr3t").env("RLT_LANE_POLL_MS", "300").output_for(600).unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{said}");
    assert_eq!(git(&f.project, &["log", "--reverse", "--format=%s", "--grep=^rlt: join"]), "rlt: join units a\nrlt: join units b");
    let calls = fs::read_to_string(state.join("calls")).unwrap();
    assert!(calls.contains("-e LANE_SECRET") && calls.contains("--network none") && calls.contains("fake-image rlt lane --spec"), "{calls}");
    assert!(!calls.contains("s3cr3t"), "the value is on no command line: {calls}");
    assert!(calls.contains("docker rm "), "the container is removed when the clone is back");
}

/// The remote-session example is a valid process (it needs a host to run).
#[test]
fn the_remote_session_example_checks() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/remote-session/process.yaml");
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).arg("process").arg(&file).arg("--check").output_bounded().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
}

/// What `--check` refuses of a target: a kind without what it needs, a setting of another kind.
#[test]
fn a_target_without_what_its_kind_needs_is_refused() {
    let f = fixture(&[("parallel: { max: 4 }", "parallel: { max: 2, target: { kind: ssh, user: deploy } }\n    #"),], &[], None);
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).arg("process").arg(&f.process).arg("--check").output_bounded().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && said.contains("`ssh` target needs `host`") && said.contains("needs `dir`"), "{said}");
    let g = fixture(&[("parallel: { max: 4 }", "parallel: { max: 2, target: { kind: container, host: x } }\n    #"),], &[], None);
    let out = Command::new(env!("CARGO_BIN_EXE_rlt")).arg("process").arg(&g.process).arg("--check").output_bounded().unwrap();
    let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && said.contains("`target.host` doesn't belong to a `container` target"), "{said}");
}
