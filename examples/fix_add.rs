//! The README's first workflow, in Rust: an agent fixes `add.py`, a verifier Relentless runs itself decides whether it is fixed,
//! a failed attempt goes back with its reason, and a human approves the result. The same job as `examples/hello/process.yaml`.
//!
//! Run it in a git repository that holds `add.py` and `rlt/verifiers/tests.sh` (copy `examples/hello/projects/demo`
//! and move `examples/hello/verifiers/tests.sh` to `rlt/verifiers/`):   cargo run --example fix_add

use relentless::prelude::*;

fn main() -> std::process::ExitCode {
    relentless::driver::run(|| {
        let rlt = Relentless::open(".")?;
        let tests = Verifier::script("tests", "rlt/verifiers/tests.sh");
        let fix = Step::new("fix").prompt("Fix add.py so that `python3 add.py` prints 3. Change only add.py.");

        let mut news = "This is the first attempt.".to_string();
        for attempt in 1..=5 {
            rlt.check_limits()?;
            rlt.step(&fix.clone().news(news))?;      // a fresh agent session, staged on its own branch
            let verdict = rlt.verifier(&tests);      // Relentless runs the verifier, never the agent
            if verdict.met() {
                rlt.gate()?;                         // keep the work: merge the branch
                let approved = rlt.hil_bool("add.py is fixed. Approve?")?;   // a human decides
                println!("approved: {approved}");
                return Ok(());
            }
            news = format!("Attempt {attempt} did not pass: {}", verdict.rationale);   // the reason goes back
        }
        rlt.block("add.py is still wrong after five attempts: a human is needed")?;
        Ok(())
    })
}
