//! Parallel work the Rust way: `fork()` gives an instance of the engine in a worktree of its own, `merge()` brings its work
//! back, and the parallelism is any Rust you like: `rayon::join`, `std::thread::scope`, a thread pool. The steps here are plain
//! functions that write a file, so no tokens are spent (the engine still wants an agent CLI on `PATH`, like any run).
//!
//!     cargo run --example forks

use rayon::prelude::*;
use relentless::prelude::*;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").arg("-C").arg(dir)
        .args(["-c", "user.name=demo", "-c", "user.email=demo@example.com", "-c", "commit.gpgsign=false"]).args(args).status().unwrap().success();
    assert!(ok, "git {args:?} failed in {}", dir.display());
}

/// Stands for a step that makes something: a file, committed in the fork's worktree (`dir()` is the worktree).
fn write(fork: &Relentless, file: &str) -> Result<(), Fatal> {
    std::fs::write(fork.dir().join(file), format!("{file}\n"))?;
    git(fork.dir(), &["add", "-A"]);
    git(fork.dir(), &["commit", "-qm", file]);
    Ok(())
}

fn main() -> std::process::ExitCode {
    relentless::driver::run(drive)
}

fn drive() -> Result<(), Fatal> {
    let dir = std::env::temp_dir().join(format!("rlt-forks-example-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README"), "demo\n")?;
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "start"]);

    let mut rlt = Relentless::open(&dir)?;

    // Different steps at once: a fork each, run with rayon, then merged in the order you choose.
    let (guide, slides) = (rlt.fork()?, rlt.fork()?);
    let (a, b) = rayon::join(|| write(&guide, "guide.md"), || write(&slides, "slides.md"));
    a?;
    b?;
    rlt.merge(guide)?;
    rlt.merge(slides)?;

    // The same with the standard library only.
    let (notes, faq) = (rlt.fork()?, rlt.fork()?);
    let (a, b) = std::thread::scope(|s| {
        let (a, b) = (s.spawn(|| write(&notes, "notes.md")), s.spawn(|| write(&faq, "faq.md")));
        (a.join().unwrap(), b.join().unwrap())
    });
    a?;
    b?;
    rlt.merge(notes)?;
    rlt.merge(faq)?;

    // The best of three tries: each is checked, one is merged, and the others are dropped, which discards them (their branches stay).
    let tries = [rlt.fork()?, rlt.fork()?, rlt.fork()?];
    tries.par_iter().enumerate().for_each(|(i, t)| write(t, if i == 1 { "ok.txt" } else { "no.txt" }).unwrap());
    let best = tries.into_iter().find(|t| t.dir().join("ok.txt").exists()).expect("one try passes");
    rlt.merge(best)?;

    // Many items, at most four at once: the pool's size is the limit.
    let units = ["a", "b", "c", "d", "e", "f"];
    let forks = units.iter().map(|_| rlt.fork()).collect::<Result<Vec<_>, _>>()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    pool.install(|| forks.par_iter().zip(&units).for_each(|(f, u)| write(f, &format!("unit-{u}.txt")).unwrap()));
    for f in forks {
        rlt.merge(f)?;
    }

    println!("{}", String::from_utf8_lossy(&Command::new("git").arg("-C").arg(&dir).args(["log", "--oneline", "--merges"]).output()?.stdout));
    drop(rlt);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
