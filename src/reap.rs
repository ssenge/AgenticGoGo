//! Straggler reaping — kill any process left behind by a worker session.
//!
//! The worker is spawned as its own process-group LEADER (`process_group(0)`), so its
//! process-group id (pgid) equals the worker pid. Every child it spawns inherits that pgid
//! and KEEPS it across reparenting — even a `nohup … &` orphan whose parent died stays in
//! the worker's group (backgrounding does not call `setsid`). So the pgid is a stable,
//! env-free, cross-platform marker for "spawned by this session".
//!
//! `run_session` already does one `kill(-pgid)` when the worker exits, which reaps children
//! that are still alive at that instant. But a child can outlive that single signal (slow to
//! die, or spawned right at the boundary). [`reap_pgid`] does a second, explicit sweep: it
//! LISTS every process currently in the group and SIGKILLs each — catching stragglers the
//! one-shot group-kill missed. pgid is freely queryable on every OS (`ps -o pgid=` on
//! unix; CIM on Windows), unlike a process's environment, which modern macOS blocks reading
//! for other users' / hardened processes.
//!
//! INTENTIONAL long tasks must NOT be reaped. A worker can deliberately leave a process
//! running past its turn (a multi-hour sim it will poll in a later session) via `agg spawn`,
//! which records the task's pgid in the spawn registry. [`reap_pgid_except`] takes that set
//! of PROTECTED pgids and spares them — so the straggler sweep still kills real leaks but
//! never the legitimate background work. (Caveat to the "children keep the worker's pgid"
//! note above: a task that detaches via `setsid` gets its OWN new group and escapes the
//! worker group entirely — that is exactly the orphan class the protected-pgid registry +
//! the boundary scanner exist to track, since the worker-group sweep alone cannot see it.)
//!
//! (We deliberately do NOT rely on reading `/proc/<pid>/environ` or `ps -E` env output — the
//! latter is restricted on current macOS and returns nothing, which silently defeats an
//! env-marker scheme. pgid avoids that trap entirely.)

use std::collections::HashSet;

/// Kill every process in process group `pgid` (other than `self_pid`). Best-effort: a process
/// may exit between listing and killing. Returns the number of stragglers it signalled.
/// (Retained as the no-exemption convenience + the tested entry point; the loop now always
/// goes through [`reap_pgid_except`] with the spawn registry's protected set.)
#[allow(dead_code)]
pub fn reap_pgid(pgid: u32) -> usize {
    reap_pgid_except(pgid, &HashSet::new())
}

/// Like [`reap_pgid`], but SPARE any process whose own pgid is in `protected`. This is how
/// an `agg spawn` long task survives the worker's post-session straggler sweep: the registry
/// records the task's pgid as protected, and the reaper skips it instead of killing legitimate
/// background work along with real leaks. A process is spared iff ITS pgid is protected, so a
/// detached task that escaped into its own group is matched correctly even after re-parenting.
pub fn reap_pgid_except(pgid: u32, protected: &HashSet<u32>) -> usize {
    let self_pid = std::process::id();
    // If the worker's own group is protected wholesale, nothing to do.
    if protected.contains(&pgid) {
        return 0;
    }
    let pids: Vec<u32> = pids_in_group(pgid)
        .into_iter()
        .filter(|&p| p != self_pid)
        // skip any pid that belongs to a protected group (a registered long task that
        // happens to still sit in the worker's group, or whose own group is protected).
        .filter(|&p| group_of(p).map(|g| !protected.contains(&g)).unwrap_or(true))
        .collect();
    let mut n = 0;
    if protected.is_empty() {
        // Fast path: no exemptions → a single group-level SIGKILL is correct and atomic.
        group_kill(pgid);
    }
    // Per-pid kill (the only correct path when exemptions exist: a group_kill cannot spare
    // a member). Idempotent with the group_kill above when protected is empty.
    for pid in pids {
        if pid_alive(pid) {
            kill_pid(pid);
        }
        n += 1;
    }
    n
}

/// pgid of a single process, or None if it's gone. Used to honor the protected set per-pid.
#[cfg(unix)]
fn group_of(pid: u32) -> Option<u32> {
    let o = std::process::Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}
#[cfg(not(unix))]
fn group_of(_pid: u32) -> Option<u32> {
    None
}

// ---------------- list pids in a process group ----------------

#[cfg(unix)]
fn pids_in_group(pgid: u32) -> Vec<u32> {
    // `ps -o pid=,pgid=` → one "pid pgid" line per process. Match the group.
    let mut out = Vec::new();
    let Ok(o) = std::process::Command::new("ps").args(["-A", "-o", "pid=,pgid="]).output() else {
        return out;
    };
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(pg)) = (it.next(), it.next()) else { continue };
        if let (Ok(pid), Ok(pg)) = (pid.parse::<u32>(), pg.parse::<u32>()) {
            if pg == pgid {
                out.push(pid);
            }
        }
    }
    out
}

#[cfg(not(unix))]
fn pids_in_group(pgid: u32) -> Vec<u32> {
    // Windows has no POSIX process groups. We model a "group" as the job started under the
    // worker; `taskkill /T` (tree kill on the worker pid) does the real reaping there, so this
    // returns empty and `group_kill` handles it via the tree.
    let _ = pgid;
    Vec::new()
}

// ---------------- kill a whole group ----------------

#[cfg(unix)]
fn group_kill(pgid: u32) {
    // kill(-pgid, SIGKILL): signal the entire process group at once.
    unsafe {
        libc_kill(-(pgid as i32), 9);
    }
}

#[cfg(not(unix))]
fn group_kill(pgid: u32) {
    // On Windows pgid carries the worker pid; /T kills the whole process tree.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pgid.to_string(), "/T", "/F"])
        .output();
}

// ---------------- single-pid helpers ----------------

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe { libc_kill(pid as i32, 0) == 0 }
}
#[cfg(unix)]
fn kill_pid(pid: u32) -> bool {
    unsafe { libc_kill(pid as i32, 9) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}
#[cfg(not(unix))]
fn kill_pid(pid: u32) -> bool {
    std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaping_an_empty_group_kills_nothing() {
        // a pgid no process belongs to → zero stragglers, no panic.
        assert_eq!(reap_pgid(2_000_000_000), 0);
    }

    #[cfg(unix)]
    #[test]
    fn reaps_stragglers_in_the_worker_group() {
        use std::process::{Command, Stdio};
        use std::os::unix::process::CommandExt;
        // Spawn a "worker" that is its own group leader (pgid == its pid), which itself
        // backgrounds a long-lived child (a straggler) that stays in the group. Then the
        // worker exits, orphaning the child — exactly the leak scenario.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "nohup sleep 60 >/dev/null 2>&1 & echo $!; exit 0"])
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        unsafe { cmd.pre_exec(|| { libc_setsid_safe(); Ok(()) }); }
        let out = cmd.output().expect("spawn worker");
        let child_pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
        assert!(child_pid > 0, "couldn't read straggler pid");
        std::thread::sleep(std::time::Duration::from_millis(200));
        // The worker (group leader) pid is the straggler's pgid (it inherited the group).
        let pgid = group_of(child_pid).expect("straggler has a pgid");
        assert!(pid_alive(child_pid), "straggler should be alive before reap");
        let n = reap_pgid(pgid);
        assert!(n >= 1, "should have reaped >=1 straggler in the group");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!pid_alive(child_pid), "straggler {child_pid} must be dead after reap_pgid");
    }

    #[cfg(unix)]
    fn group_of(pid: u32) -> Option<u32> {
        let o = std::process::Command::new("ps").args(["-o", "pgid=", "-p", &pid.to_string()]).output().ok()?;
        String::from_utf8_lossy(&o.stdout).trim().parse().ok()
    }

    #[cfg(unix)]
    #[test]
    fn reap_except_spares_a_protected_group() {
        use std::process::{Command, Stdio};
        use std::os::unix::process::CommandExt;
        // Two independent `sleep` processes, EACH its own group leader (setsid → pgid==pid),
        // and each ORPHANED to init so the OS — not this test process — reaps its zombie when
        // killed (otherwise a SIGKILL'd direct child lingers as a defunct that `kill(pid,0)`
        // still reports alive, which would mask the real kill). We launch via an intermediate
        // `sh` that setsids the sleep, prints its pid, and exits → sleep reparents to pid 1.
        let spawn_leader = || -> u32 {
            use std::io::Read;
            // sh becomes a new group leader via pre_exec(setsid) — portable (the setsid
            // *binary* doesn't exist on macOS, but the syscall does). It backgrounds a sleep
            // (which inherits sh's new group), echoes the sleep's pid, then exits → the sleep
            // reparents to pid 1 in that group, and init reaps its zombie when we kill it.
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "sleep 30 >/dev/null 2>&1 & echo $!"])
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
            unsafe { cmd.pre_exec(|| { libc_setsid_safe(); Ok(()) }); }
            let mut child = cmd.spawn().expect("spawn sh launcher");
            let mut s = String::new();
            child.stdout.take().unwrap().read_to_string(&mut s).ok();
            let _ = child.wait(); // reap the sh; the sleep is now an init-owned orphan
            s.trim().parse().expect("sleep pid")
        };
        let worker_pid = spawn_leader();
        let prot_pid = spawn_leader();
        std::thread::sleep(std::time::Duration::from_millis(200));
        // each `setsid sleep` is its own group leader → pgid == pid.
        let worker_pgid = group_of(worker_pid).unwrap_or(worker_pid);
        let prot_pgid = group_of(prot_pid).unwrap_or(prot_pid);
        assert!(pid_alive(worker_pid) && pid_alive(prot_pid), "both must start alive");
        let mut protected = HashSet::new();
        protected.insert(prot_pgid);
        // sweep the WORKER group, protecting the long-task group.
        reap_pgid_except(worker_pgid, &protected);
        std::thread::sleep(std::time::Duration::from_millis(250));
        assert!(!pid_alive(worker_pid), "unprotected worker-group leader must be reaped");
        assert!(pid_alive(prot_pid), "PROTECTED long-task must SURVIVE the sweep");
        kill_pid(prot_pid); // cleanup the spared one
    }

    #[cfg(unix)]
    fn libc_setsid_safe() {
        unsafe { libc_setsid(); }
    }
    #[cfg(unix)]
    extern "C" {
        #[link_name = "setsid"]
        fn libc_setsid() -> i32;
    }
}
