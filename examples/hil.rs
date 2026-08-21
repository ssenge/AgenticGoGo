//! SAMPLE (PLAN) — every human-in-the-loop feature from `internal/HUMAN_LOOP.md`, in one driver.
//!
//! ✅ **THIS COMPILES.** It is a cargo example, which is what keeps it honest: it was written as the
//! SPECIFICATION of the HiL surface before any of it existed, and it is now the acceptance test for
//! it — the same role `examples/workflow.rs` plays for the driver API. It is not meant to be RUN
//! as-is (it names judge files and a project that do not exist here); every call and signature below
//! is the real one, checked by the compiler on every build.
//!
//! Writing it first paid for itself: it surfaced four gaps in the design. Two were real and shipped
//! (`work_time` as a `Limits` field, and `open_asks()`); two were cut on inspection. Both outcomes
//! are recorded at the bottom, because "the sample needed API the plan lacked" is the cheapest
//! design review there is.
//!
//! WHAT IT DEMONSTRATES — the eight cases of HUMAN_LOOP.md §3, in the three calls of §4.5:
//!   B decide      `hil_choose`  — which store, when the agent may not pick
//!   C provide     `hil_input`   — a value only a human has, validated by a judge
//!   C′ secret     `hil_bool`    — NEVER `hil_input`; the human places it, agg confirms
//!   D act         `hil_bool`    — a real-world change, verified against the WORLD in a retry loop
//!   E take over   `hil_bool`    — after `gate()`, never before
//!   A authorize   `hil_bool`    — the prod deploy, which blocks until a human says yes
//!   F accept      `Judge::human`— a sign-off that BINDS `done_if`, not just a branch
//!   G intervene   (nothing)     — already shipped: `agg send inject|budget|pause|stop`
//!   + worker-initiated asks, which never block the loop
//!
//! NARRATIVE: ship a billing service to production. Half the work is code; the other half is a
//! human with a credit card, a DNS console and an opinion.

use agg::prelude::*;

// `-> ExitCode`, not `-> Result<(), Fatal>`: Rust's `Termination` impl collapses every ending to
// exit 1, so a driver returning a `Result` reports `agg stop` (5), a blown ceiling (3) and a genuine
// panic identically. `agg::driver::run` maps the ending to the codes `agg run` uses.
fn main() -> std::process::ExitCode {
    agg::driver::run(real_main)
}

fn real_main() -> Result<(), Fatal> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());

    let agg = Agg::open_with(dir, Opts { resume: false })?
        // ═══ CEILINGS, IN SECONDS (HUMAN_LOOP §7.4) ═══════════════════════════════════════════
        // The rename that matters: `wall_hours` is gone and `wall_time` is SECONDS. A config or
        // driver that mechanically renames `8` to `8` gets a run that aborts after eight seconds,
        // which is why the old key is a hard error at startup rather than an alias.
        //
        // GAP 1, now shipped: `work_time` is a `Limits` FIELD, not only a YAML condition term. On
        // the Rust path ceilings are fields, and `work_time` is the one a HiL driver actually wants.
        //
        //   wall_time  e2e, human waiting INCLUDED   → a DEADLINE
        //   work_time  wall_time − human_wait_time   → an EFFORT ceiling
        //
        // Why a HiL driver must set `work_time` and not only `wall_time`: this file blocks
        // indefinitely (§4.5.4) and ceilings keep firing while blocked. With a deadline alone, one
        // overnight question — asked at 23:00, answered at 08:00 — burns nine hours of a ceiling
        // that was measuring the agent's effort, and a healthy run dies because a person slept.
        .limits(Limits {
            tokens:    Some(40_000_000),
            cost:      None,
            sessions:  Some(400),
            wall_time: Some(7.0 * 24.0 * 3600.0),   // 7 days e2e — humans are in this loop
            work_time: Some(8.0 * 3600.0),          // 8h of ACTUAL looping
        })
        .on_regression(OnRegression::Annotate);

    drive(&agg)
}

fn drive(agg: &Agg) -> Result<(), Fatal> {
    // Steps and judges are the ordinary surface — see `examples/workflow.rs` for the full treatment
    // of templates, isolation and the three judge kinds. Only what HiL needs is spelled out here.
    let build = Step::template().agent(Agent::Claude).isolation(Isolation::Sandbox)
        .readonly(["tests/", "agg/judges/"]);

    let scaffold  = build.create("scaffold").prompt("Scaffold the billing service against agg/state/wiki/spec.md.");
    let migrate   = build.create("migrate").prompt("Write and run the schema migration. Commit when green.");
    let implement = build.create("implement").prompt("Implement charge + refund against the spec. Commit when green.");
    let deploy    = Step::new("deploy").isolation(Isolation::None)
        .prompt("Run ./deploy.sh against the configured environment. Do NOT change infra by hand.");

    let tests     = Judge::script("tests_pass",  "agg/judges/tests.sh");
    let lint      = Judge::script("lint_clean",  "agg/judges/lint.sh");
    let migrated  = Judge::script("migrated",    "agg/judges/migrated.sh");
    let progressing = Judge::script("progressing", "agg/judges/progressing.sh"); // met = GOOD, see below
    let dsn_ok    = Judge::script("dsn_ok",      "agg/judges/dsn_reachable.sh");
    let key_ok    = Judge::script("key_present", "agg/judges/stripe_key.sh");
    let dns_ok    = Judge::script("dns_ok",      "agg/judges/dns_resolves.sh");

    // ⚠ `stalled` ships INVERTED (met = stalled = bad). `gate()`'s regression rule is "was met, now
    // unmet ⇒ worse", so a met-when-bad judge must be flipped before a driver uses it. Hence
    // `progressing` — a three-line shadow that negates the library judge. RUST_API.md §"Two rules".

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // 0 · THE CONTRAST THAT DEFINES THE FEATURE
    //
    //   agg.ask(msg)         non-blocking. Pages a human, KEEPS RUNNING. Shipped today. Use it
    //                        whenever the loop has other useful work — which is most of the time.
    //   agg.hil_bool(msg)?   BLOCKS until a human answers. No timeout, no default, no ending the
    //                        run (§4.5.4). Use it only when the next line of code genuinely cannot
    //                        be written without the answer.
    //
    // The second one is the whole feature and also the whole risk: agg exists because a raw coding
    // agent stops every few minutes to ask permission. What keeps `hil_*` from being that is not a
    // bound on the wait — there is none — it is WHO may open one: a driver author, at a call site
    // they wrote, in a file under review. The worker cannot reach these functions (§5.6).
    //
    // Escape hatch, and the reason no timeout is needed: a block drains the operator bus while it
    // waits, so `agg stop` (or Ctrl-C) ends a run nobody intends to answer.
    // ═════════════════════════════════════════════════════════════════════════════════════════

    agg.info("starting the billing-service run — expect ~6 questions");

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // 1 · DECIDE (case B) — `hil_choose`: a closed answer set
    //
    // The agent could pick a store. It may not: this one outlives the run and someone owns the bill.
    //
    // Why `choose` and not `input`: the options are recorded in `agg/private/asks.jsonl`, so
    // `agg send answer <id> 3` is REJECTED at the CLI with the list re-printed, and the ask stays
    // open. The driver cannot be handed a value it did not offer — a boundary property, not
    // ergonomics. `hil_input` is the open-set call and needs a judge instead (§2 below).
    // ═════════════════════════════════════════════════════════════════════════════════════════
    let stores = ["postgres-rds", "postgres-neon", "sqlite-litefs"];
    let store = stores[agg.hil_choose("Which store for billing? (it outlives this run)", &stores)?];
    agg.log(&format!("store chosen: {store}"));

    agg.step(&scaffold.clone().prompt(format!(
        "Scaffold the billing service against agg/state/wiki/spec.md. Target store: {store}."
    )))?;
    agg.gate()?;

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // 2 · PROVIDE (case C) — `hil_input`, then VERIFY IT
    //
    // An open-set answer is a typo waiting to happen, so the pattern is always ask-then-check.
    // Note the loop: a rejected value re-asks with the failure in the question, which is the only
    // way the human learns what was wrong with the last one.
    // ═════════════════════════════════════════════════════════════════════════════════════════
    let mut why = String::new();
    let dsn = loop {
        let answer = agg.hil_input(&format!(
            "{why}Which {store} instance is PROD? (host:port/db, reachable from this machine)"
        ))?;
        std::env::set_var("BILLING_DSN", &answer);      // the judge reads it from the env
        if agg.judge(&dsn_ok).met() { break answer; }
        why = format!("`{}` did not answer: {}. ", answer, agg.judge(&dsn_ok).rationale());
    };
    agg.log(&format!("prod DSN accepted: {dsn}"));

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // 3 · SECRETS (case C′) — the one place `hil_input` is FORBIDDEN
    //
    // `hil_input("Paste the Stripe key")` builds a credential logger. The value would land in
    // `asks.jsonl`, in `INSTRUCTIONS.md`, in `run.log`, and — because the bus is files on disk —
    // in `agg/private/bus/in/`. No newtype fixes that; the fix is not to carry the value at all.
    //
    // So the human puts the secret where secrets go and agg learns only THAT it is there. The rule,
    // stated once: **an answer may NAME a secret, never CONTAIN one.**
    // ═════════════════════════════════════════════════════════════════════════════════════════
    while !agg.judge(&key_ok).met() {
        agg.hil_bool("Put STRIPE_KEY (live mode) in this run's environment or ~/.billing.env. Done?")?;
        // The `while` re-runs the judge, so a "yes" that was not true only costs one more question.
        // A human's "done" is a CLAIM; the judge is what makes it a FACT. This is the shape that
        // replaced the plan's earlier `hil_act(what, &judge)` — the judge is a line, not a signature.
    }

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // 4 · A REAL-WORLD ACT (case D) — verify against the WORLD, never the claim
    //
    // Nothing agg can run creates this DNS record. The human does it in a console, and the judge
    // resolves the name from outside — not "did the human touch a file", which is the mistake that
    // makes this case dangerous.
    // ═════════════════════════════════════════════════════════════════════════════════════════
    let mut attempt = 0;
    while !agg.judge(&dns_ok).met() {
        attempt += 1;
        let q = if attempt == 1 {
            "Create the A record billing.prod → the LB, and pierce the firewall for :443. Done?".to_string()
        } else {
            format!("Still not resolving (attempt {attempt}): {}. Fixed?", agg.judge(&dns_ok).rationale())
        };
        agg.hil_bool(&q)?;
    }
    agg.info("billing.prod resolves — infra prerequisites are real, not claimed");

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // 5 · THE WORK, and TAKE OVER when it fails (case E)
    //
    // ⚠ THE ONE PLACEMENT RULE IN THIS FILE: `gate()` BEFORE blocking for a human who will touch
    // the tree. `step()` only STAGES — work sits on a session branch until a gate lands it — so a
    // human editing the tree while a span is open is a merge conflict at 3am. agg knows a span is
    // open and WARNS at the blocking call, but the warning is a net, not a design.
    //
    // Note what is NOT here: no forced exit. An earlier draft had `hil_takeover()` end the run,
    // which was wrong — an attached human should take the task, finish it, and let the loop carry
    // on with its state intact. Ending stays available as `agg stop`.
    // ═════════════════════════════════════════════════════════════════════════════════════════
    for attempt in 1..=3 {
        agg.check_limits()?;                    // ceilings are OPT-IN — this is where they fire
        agg.step(&migrate)?;
        if agg.judge(&migrated).met() { break; }
        agg.info(&format!("migration attempt {attempt}/3 failed: {}", agg.judge(&migrated).rationale()));
    }
    if !agg.judge(&migrated).met() {
        agg.gate()?;                            // ← land whatever exists BEFORE a human touches it
        while !agg.judge(&migrated).met() {
            agg.hil_bool("3 attempts, migration still red. Take it over and confirm when done?")?;
        }
        agg.info("human landed the migration — resuming autonomously");
    }

    let cycle = agg.pos("implement", 20);
    for c in 1..=20 {
        cycle.update(c);
        agg.check_limits()?;
        agg.step(&implement)?;

        if agg.judge(&tests).met() && agg.judge(&lint).met() {
            agg.gate()?;
            break;
        }
        // NON-BLOCKING, deliberately: the loop has 19 more cycles of useful work, so this is `ask`,
        // not `hil_bool`. Reaching for the blocking call here is how a driver becomes an interactive
        // agent one call site at a time.
        if !agg.judge(&progressing).met() {
            agg.ask("implementation has not moved in 3 cycles — a different approach worth trying?");
        }
        agg.gate()?;
    }

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // 6 · WORKER-INITIATED ASKS — the loop never blocks on these
    //
    // The worker discovers things a driver author cannot predict ("this SDK needs a licence key").
    // It has its own front-end — `agg hil bool|choose|input "<q>"` — which RECORDS and EXITS: a
    // worker session is a paid subprocess holding a git branch, so it must never wait. The answer
    // reaches the NEXT session's `agg/private/INSTRUCTIONS.md`, scoped to the ask id.
    //
    // GAP 2, now shipped: `open_asks()`. A driver never learns the id of an ask the worker opened —
    // `agg hil` mints it in another process — so without a reader those questions are invisible to
    // the flow that has to react to them.
    for ask in agg.open_asks() {
        agg.info(&format!(
            "the worker is waiting on [{}] {} ({}s)",
            ask.id,
            ask.question,
            ask.age_secs(agg::util::now_epoch())
        ));
        // A driver MAY block on the worker's question — and that choice being the AUTHOR's, here, at
        // a call site, is what keeps the asymmetry honest. The worker asked; the author decided to
        // wait. Nothing the worker can write makes the loop stop.
        //
        // GAPS 3 and 4 were CUT on inspection: `Ask::blocks_progress()` (agg cannot know whether a
        // question blocks the work — the author can read it) and `hil_answer()` (letting driver code
        // satisfy a question addressed to a human is a hole in the premise, not a feature).
    }

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // 7 · AUTHORIZE (case A) — the reason the block has no timeout
    //
    // The fail-safe is the block itself: while nobody answers, the deploy simply does not happen.
    // An earlier draft had `hil_auth_required` with "timeout ⇒ deny", which is the same behaviour
    // reached through a policy knob and a second function name.
    //
    // A denial is not an error — the human said no, which is a legitimate outcome of a workflow
    // that asked. Return cleanly; `agg stop`'s exit 5 is for an operator abandoning the run, and
    // `Halt`'s 3 is for a ceiling. A refused deploy is neither.
    // ═════════════════════════════════════════════════════════════════════════════════════════
    if !agg.hil_bool(&format!(
        "Deploy billing v{} to PROD? store={store}, dsn={dsn}, tests green, DNS live.",
        env!("CARGO_PKG_VERSION")
    ))? {
        agg.info("deploy declined by a human — stopping with the work landed and nothing shipped");
        return Ok(());
    }
    agg.step(&deploy)?;
    agg.gate()?;

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // 8 · ACCEPT (case F) — sign-off as a blocking question
    //
    // ⚠ `Judge::human` — a human's answer becoming a VERDICT ROW that counts in `done_if` and the
    // `N/M` scoreboard — is Stage 3 and NOT BUILT. It needs two rules the gate does not have yet
    // (cache the answer on the graded artefact's commit sha; exempt an unanswered human judge from
    // the regression rule), or it becomes a pager loop: a run-set judge is re-evaluated after every
    // judged step. See `internal/HUMAN_LOOP.md` §7.3.
    //
    // Until then, case F is a `hil_bool` before the final gate. What you lose is only the sign-off
    // appearing in `N/M`; what you keep is the gate.
    //
    // Worth it because a human is the ONE grader the worker cannot forge: the request is
    // worker-authored and untrusted, but the ANSWER arrives on `agg/private/bus/`, carved out of the
    // sandbox's writable set. That makes it the strongest row of the moat table (HUMAN_LOOP §4.1).
    //
    // Two rules make it safe, and both live in the implementation, not here (§7.3):
    //   · the answer is CACHED ON THE GRADED ARTEFACT'S COMMIT SHA — so the human is asked once per
    //     CHANGE, not once per step, and a stale yes can never approve code nobody saw;
    //   · an UNANSWERED human judge is EXEMPT from the regression rule — otherwise "was met, now
    //     unmet" rolls back the very work it wants reviewed, and the run deadlocks.
    //
    // And because a human is the most expensive judge there is, it goes LAST in the `&&` chain:
    // `&&` short-circuits, so nobody is paged until the cheap machine checks already pass. This is
    // the same cost gate `examples/workflow.rs` uses for a 40-minute load test.
    // ═════════════════════════════════════════════════════════════════════════════════════════
    let smoke = Judge::script("smoke_ok", "agg/judges/smoke_prod.sh");

    // `&&` SHORT-CIRCUITS, and that is the cost gate: the human is the most expensive judge there
    // is, so nobody is paged until the cheap machine checks already pass. Same pattern
    // `examples/workflow.rs` uses to keep a 40-minute load test off the critical path.
    if agg.judge(&tests).met() && agg.judge(&smoke).met() {
        if !agg.hil_bool("Prod smoke is green. Sign off on the release?")? {
            agg.info("sign-off declined — the release is staged, not live");
            return Ok(());
        }
        agg.info("released");
        return Ok(());
    }

    agg.ask("prod smoke did not pass — the release is staged, not live");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// THE HiL SURFACE, ENTIRE
//
//   agg.hil_bool(q)            -> Result<bool, Fatal>     blocks until answered
//   agg.hil_choose(q, &opts)   -> Result<usize, Fatal>    blocks; closed answer set, CLI-validated
//   agg.hil_input(q)           -> Result<String, Fatal>   blocks; open set — pair it with a judge
//   Judge::human(name, q)      -> Judge                   a verdict row; binary
//   Judge::human(..).scored(t) -> Judge                   value from `hil_input`, met = value >= t
//
//   Limits { .., wall_time, work_time }   SECONDS. `work_time` excludes human wait — the thing
//                                          that makes an indefinite block survivable.
//
//   agg.ask(msg) / info(msg) / log(msg)   the shipped non-blocking levels. STILL THE DEFAULT.
//   agg send answer|approve|deny <id>     how a human replies, from anywhere that has a shell
//   agg hil bool|choose|input "<q>"       the WORKER's front-end: records, exits, never waits
//
// FOUR RULES, AND THEY ARE THE WHOLE DESIGN
//   1. A human's answer unblocks the STEP; a JUDGE still owns the VERDICT. Every case above that
//      claims something about the world is checked, not trusted.
//   2. An answer may NAME a secret, never CONTAIN one.
//   3. `gate()` before you block for a human who will touch the tree.
//   4. Only a driver author may open a block. Not the worker, not `agg.yaml`. That asymmetry is the
//      only thing standing between this feature and the interactive agent agg exists to replace.
//
// ⚠ WHAT WRITING THIS FILE SURFACED — four additions the plan must absorb before Stage 1:
//   (1) `work_time` (and `wall_time`) as `Limits` FIELDS, not only YAML condition terms. §7.4
//       specifies the terms; the Rust path carries ceilings as fields and a HiL driver needs both.
//   (2) `agg.open_asks() -> Vec<Ask>` — a driver cannot see a worker-initiated ask at all today.
//       Without it, cases B/C/D discovered by the worker are invisible to the flow that reacts.
//   (3) `Ask::blocks_progress()` — or something that lets a driver distinguish "the worker is
//       curious" from "the worker is stuck". Possibly just the `case` field; possibly not needed,
//       in which case delete it and let the author decide from the question text.
//   (4) `agg.hil_answer(&ask)` — answering an ask the DRIVER did not open. Unclear whether this is
//       wanted at all: it lets driver code satisfy a worker's question, which may be a useful
//       automation or may be a hole in "a human answers". DECIDE BEFORE IMPLEMENTING.
//
// ⚠ AND ONE THING TO CHECK ON THE WIRE, per the house rule that agent behaviour is verified by
//   running it: that a blocked `hil_*` really does keep draining the bus, so `agg stop` interrupts
//   a wait. The whole no-timeout decision rests on it.
