//! `Relentless::fan_out`: the keys of a list run at once as lanes, each in a worktree of its own, and are joined in key
//! order. Each lane here is a plain function; a driver with agents calls `lane.step(..)` in it.
//!
//! Run it in a git repository:   cargo run --example fanout -- <project>

use relentless::prelude::*;

fn main() -> Result<(), Fatal> {
    let project = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let rlt = Relentless::open(&project)?;
    let reports = rlt.fan_out(&["billing", "orders", "search"], FanOut::new().max(3), |lane, key| {
        // a driver with agents would run steps here: lane.step(&Step::new(format!("port {key}")).prompt("..."))?;
        let file = lane.dir().join(format!("{key}.txt"));
        std::fs::write(&file, format!("the {key} module\n"))?;
        let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(lane.dir()).args(args).status();
        git(&["add", "-A"])?;
        git(&["-c", "user.name=lane", "-c", "user.email=lane@example.com", "commit", "-qm", &format!("port {key}")])?;
        Ok(())
    })?;
    for r in &reports {
        println!("{}: {:?}", r.key, r.end);
    }
    Ok(())
}
