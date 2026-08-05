//! SAMPLE — "Recursively Self-Improving Software": a workflow in which agg improves **agg**.
//!
//! ✅ **THIS COMPILES.** It is a cargo example (`cargo build --example selfimprove`). Like its
//! sibling `examples/workflow.rs` it is not meant to be RUN as-is — the judge files it names are
//! illustrative — but every call and type below is compiler-checked against the shipped API.
//!
//! REVISION 4 (2026-08-05): compiled for the first time against the shipped API. Two changes, both
//! annotated where they happen — `MINUTES` was never a real constant, and a native judge's closure
//! is `'static`, so the judge it consults is cloned in rather than borrowed.
//!
//! REVISION 3 (2026-08-04). REWRITTEN 2026-08-03 against the rev-6 API (`examples/workflow.rs`,
//! HANDOFF §4b), amended
//! 2026-08-04: one `Agent` enum, ceilings and `on_regression` in Rust, `agg.gate()` after VERIFY,
//! `with_*` Verdict setters (the final audit's E0592 fix). The
//! previous version was written against the node model — Stage, Sequence, Action, Loop, guard,
//! `.action()`, `Status::Exhausted` — all of which are deleted. Flow here is `for`/`if`/`continue`.
//!
//! NOT HYPOTHETICAL. agg's `isolation: container` tier was built by agg driving its own source in a
//! real overnight run. Every hazard in the table at the bottom was actually present.
//!
//! ── WHY THIS SAMPLE IS THE ARGUMENT FOR DELETING `gate:` ─────────────────────────────────────
//! agg's own verification ladder is the canonical gating case, and the numbers are brutal:
//!
//!     cargo build          ~5s        }
//!     clippy -D warnings   ~15s       }  the cheap ladder — ~80 seconds
//!     cargo test           ~60s       }
//!     scripts/e2e.sh       ~10 MINUTES   the suite you cannot afford to run per step
//!
//! A 60-step run that judges everything after every step spends TEN HOURS in the e2e suite alone.
//! The old design bought that back with a `gate:` field on the judge. There is no `gate:` field
//! any more — ANYWHERE, since the 2026-08-04 owner simplification cut it from YAML too: judges are
//! LAZY and memoized per step, so `&&` in the flow short-circuits and the e2e is simply never
//! reached on the ~55 steps where the build is red. Same saving, no mechanism — see VERIFY below.
//! This loop is the evidence for the whole ruling, and it cuts both ways: it is also why a ladder
//! like this one belongs on the Rust path. Expressed in YAML, `e2e` would run after every step.
//!
//! ── THE GENERATION BOUNDARY, STATED UP FRONT ─────────────────────────────────────────────────
//! **The running binary is the PREVIOUS generation.** agg cannot hot-swap itself mid-run: the
//! process executing this loop was compiled before the worker's changes existed. Improvements land
//! for the NEXT run; the final act is merge + tag, never restart-myself.
//!
//! That is a FEATURE, not an implementation gap. A loop that rebuilt and re-exec'd itself would be
//! judging its new self with its new judges under its new gate — and if any of those three were
//! broken by the change, nothing would be left to notice. The boundary is what keeps the previous
//! generation's guardrails in force over the next one. It applies to judges too: the judge written by
//! the `author_judge` step in LAND does not grade this run. Do not "fix" this.

use agg::prelude::*;

/// `main` is the ENTRY POINT ONLY — where to run and how many generations. The sample itself (steps,
/// judges, the flow) is [`drive`], so a harness can run THIS driver against a scratch project
/// (`scripts/samples_real.sh`) instead of a copy of it that drifts.
///
///   ./selfimprove                   # cwd, 12 generations — today's values, unchanged
///   ./selfimprove /tmp/proj 2       # argv:  <dir> [cycles]
///   AGG_SAMPLE_DIR=… AGG_SAMPLE_CYCLES=2 ./selfimprove    # or env
fn main() -> Result<(), Fatal> {
    let dir = arg(1, "AGG_SAMPLE_DIR").unwrap_or_else(|| ".".to_string());
    let cycles = arg(2, "AGG_SAMPLE_CYCLES").and_then(|s| s.parse().ok()).unwrap_or(12);

    // A Rust driver configures EVERYTHING in Rust — ceilings and regression policy included. This
    // path never reads `agg.yaml`; a stray one is ignored rather than half-merged.
    let agg = Agg::open(dir)?
        // Declared here, ENFORCED by `agg.check_limits()?` at the top of each generation. No
        // condition strings: there is no `.abort_if()` on the Rust path, because a driver that
        // wants to stop on a judge writes `if agg.judge(&x).met() { return Ok(()); }`.
        .limits(Limits { tokens: Some(60_000_000), cost: None,
                         sessions: Some(300), wall_hours: Some(10.0) })
        .on_regression(OnRegression::Rollback);   // self-improvement: a regression must not land

    drive(&agg, cycles)
}

/// One positional arg, or an env var, or nothing.
fn arg(n: usize, var: &str) -> Option<String> {
    std::env::args().nth(n).or_else(|| std::env::var(var).ok()).filter(|s| !s.is_empty())
}

/// A model name a RUN may pin (`AGG_SAMPLE_HEAVY_MODEL` / `_GRIND_MODEL`). The default is the model
/// the narrative names; the override lets a real run name one that exists today without editing the
/// driver. Nothing else about the step changes.
fn model(default: &str, var: &str) -> String {
    std::env::var(var).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

/// THE SAMPLE — read from here down as if it were `main`; it was, until the split.
fn drive(agg: &Agg, cycles: usize) -> Result<(), Fatal> {
    // ═════════════════════════════════════════════════════════════════════════════════════════
    // STEPS — two templates, one per trust level. `readonly` ACCUMULATES down a chain and
    // `writable` SUBTRACTS from what accumulated, which is what makes the two grants below
    // readable: each derived step re-grants exactly the one directory it has business in.
    // ═════════════════════════════════════════════════════════════════════════════════════════

    // ONE agent enum — every backend is a coding harness. There are no naked LLMs: a model without
    // tools cannot open a file, which is exactly what `select` below has to do.
    // ⚠ THE TIER IS NOT OPTIONAL HERE. `readonly` is delivered by the OS wrapper, and the wrapper
    // only runs under `sandbox`/`container` — under the default `none` this deny list would bind
    // NOTHING and the comment below would be a lie. agg warns on `readonly` without a tier.
    let reading = Step::template()             // reads and judges; edits no source
        .agent(Agent::Claude)
        .effort(Effort::High)
        .isolation(Isolation::Sandbox)
        .readonly(["src/", "tests/", "agg/judges/"]);

    let sandboxed = Step::template()           // edits agg's OWN source
        .agent(Agent::Claude)
        .isolation(Isolation::Sandbox)
        .readonly(["tests/", "agg/judges/"]);

    // Picking a roadmap item is reading and judgement — but it is reading TWO REAL FILES, so it
    // needs a harness. What keeps it cheap and safe is the inherited deny on `src/`: the step can
    // read the whole repo and write only its note. `internal/ROADMAP.md` is real.
    let select = reading.create("select")
        .prompt("Read internal/ROADMAP.md and agg/private/LOG.md. Pick the single highest-value \
                 unstarted item completable in one overnight run. Write the choice and your \
                 reasoning to agg/state/wiki/next.md. Do NOT edit source.");

    // The design doc comes BEFORE the code, and from a DIFFERENT REVIEWER. This is how agg's own
    // features were actually built: internal/STUCK_NOTIFY.md existed before src/features/notify.rs,
    // and the implementation session found four gaps in it. The doc is what makes gaps findable.
    //
    // ⚠ A different VENDOR is the stronger version of "different reviewer" and is one word away
    // (`.agent(Agent::Codex)`), which is what this shipped until 2026-08-05. A different MODEL keeps
    // the sample runnable for a reader with a single agent installed; put the vendor back if you have
    // two. Either way the point stands: the knob is PER STEP, not per run.
    let design = Step::new("design")
        .model(model("claude-sonnet-5", "AGG_SAMPLE_REVIEW_MODEL"))
        .effort(Effort::High)
        .prompt("Turn agg/state/wiki/next.md into an implementation contract another session will \
                 build from without your context. Ground every claim in code anchors you have read.");

    let implement = sandboxed.create("implement")
        .model(model("claude-opus-4-8[1m]", "AGG_SAMPLE_HEAVY_MODEL"))
        .effort(Effort::Max)
        .writable(["tests/"])                  // it SHOULD add tests; `agg/judges/` stays denied
        .prompt("Implement the contract in the design doc. Add tests alongside.");

    // Inherits the denies and re-grants NOTHING. "Make the tests pass" has an obvious shortcut,
    // and this is the step that would take it — so `tests/` is kernel-read-only for it, and
    // `agg/judges/` is too. The prompt below is a courtesy; the deny is the enforcement.
    let repair = sandboxed.create("repair")
        .model(model("claude-sonnet-5", "AGG_SAMPLE_GRIND_MODEL"))   // the grind runs cheap
        .prompt("Make the failing tests and lints pass. Do not delete or ignore tests.");

    // The ONLY step that may write graders — a real use of `writable` subtracting. Every landed
    // feature ships with the judge that keeps it honest in later runs; that judge has to be written
    // by something, and this is it. It cannot touch source, and nothing else can touch judges.
    // Accumulate then subtract, in one step: `src/` joins the inherited denies, `agg/judges/` comes
    // back out of them. Net writable: everything but `src/`, `tests/` and `agg/private/`.
    let author_judge = sandboxed.create("author_judge")
        .readonly(["src/"])
        .writable(["agg/judges/"])             // spelled EXACTLY as the template's deny — `writable`
                                               // subtracts, and a trailing-slash mismatch subtracts
                                               // nothing while looking like it worked
        .prompt("Write a judge under agg/judges/ that fails if this feature regresses. \
                 Exit 0 = met. Do not touch src/.");

    // The step-back gets a different reviewer on the theory that whoever dug the hole is the least
    // likely to see it. ⚠ A different VENDOR is the real version of that theory — `.agent()`, one
    // word — and this is the step where it is worth the most, because a stall is exactly the failure
    // a same-family model reproduces. It is a model override here only so the sample runs with one
    // agent installed.
    let reconsider = Step::new("reconsider")
        .model(model("claude-sonnet-5", "AGG_SAMPLE_REVIEW_MODEL"))
        .prompt("Assume the current approach is wrong. Name 2-3 alternatives and pick one.");

    let land = Step::new("land")
        .prompt("Mark the item done in internal/ROADMAP.md, write a release note, tag the build.");

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // JUDGES — agg's own ladder, cheapest first. Nothing is registered and nothing runs here.
    // ═════════════════════════════════════════════════════════════════════════════════════════

    let builds = Judge::script("builds",     "agg/judges/build_ok.sh");      // ~5s
    let lint   = Judge::script("lint_clean", "agg/judges/lint_clean.sh");    // ~15s
    let tests  = Judge::script("tests_pass", "agg/judges/cargo_test.sh");    // ~60s

    // ~10 minutes. No `gate` field — the `&&` in VERIFY is the gate.
    let e2e = Judge::script("e2e", "agg/judges/e2e.sh")
        // SECONDS — there is no `MINUTES` constant; earlier revisions of this file assumed one.
        .timeout(20 * 60);                // the run-level 300s default would kill it

    // ⛔ THERE IS NO `stalled` JUDGE HERE ANY MORE, and the reason is worth more than the judge was.
    //
    // The shipped `agg/judges/stalled.sh` is MET WHEN THINGS ARE BAD — the one judge in the library
    // that inverts the convention every other one follows (`tests_pass`, `builds`, `lint_clean`,
    // `no_shrink`, `e2e`: met = good). It inverts it because YAML's `notify_if: stalled` reads
    // naturally that way. On the Rust path there is no `notify_if`, so the inversion buys nothing —
    // and it costs something real: a judge that flips met→unmet when the loop RECOVERS is read by
    // `gate()` as a regression, so an `on_regression: Rollback` driver would discard the very work
    // that escaped the stall, and `landed_met` (which reads only MERGED rows) would keep the stale
    // `true` forever, so every later gate would roll back too. The run would never merge again.
    //
    // Two fixes, and this file takes the second:
    //   (1) invert it — a `not_stalled` judge, met = good. Flipping to unmet then means "we got
    //       stuck", which IS a regression, and recovery is unmet→met, which is not. Self-healing.
    //   (2) do not make it a judge at all. Stall is a property of THIS loop's progress, the driver
    //       can see it directly, and a local variable is replay-safe by construction. Below.
    //
    // The rule that generalises: on the Rust path a judge's `met` should mean GOOD. An inverted
    // detector belongs only in `notify_if`/`abort_if`, which this path does not have.

    let design_sound = Judge::rubric("design_sound", "agg/judges/design_sound.md");

    // THE ANTI-CHEAT INVARIANT: the test count must never decrease. It is a QUALITY invariant, not
    // a security one — the security answer to "delete the failing test" is the kernel deny on
    // `tests/` above. This catches the cases the deny cannot see: a test disabled from inside a
    // writable file, a `#[ignore]`, a harness that stops collecting a module.
    //
    // Native because it needs HISTORY across steps, and reading it in Rust means no subprocess, no
    // hand-parsing of verdicts.jsonl, and a comparison the compiler checks. NOT because Rust sees
    // more than a script: script judges already get `AGG_SESSION`/`AGG_STEP` (core/judge.rs:88) and
    // can be handed whatever else they need.
    // `JudgeCtx` gives a native judge the other judges (`met/value/verdict/previous/history`) plus
    // `session()`, `step()`, `dir()`, `read()` and `diff()` — and NO clock, NO RNG, NO network, NO
    // env. That exclusion is the point: a judge that reads the wall clock returns a different
    // verdict on replay, and `--resume` would fast-forward past a divergence it cannot see.
    // ⚠ THIS IS AN ORDINARY JUDGE, ASKED IN THE FLOW (see VERIFY below). There is no `.invariants()`
    // to register it with — that key is YAML-only now, along with `abort_if`/`notify_if` — so
    // "an invariant" on the Rust path just means a judge the driver asks before it gates. It joins
    // the regression set by virtue of having been asked, which is the whole rule. Note it is
    // met = GOOD, like every other judge here; that convention is what makes the rule safe.
    //
    // ⚠ A native judge's closure is `Fn + Send + Sync + 'static` — agg keeps it for the whole run,
    // so it cannot BORROW another judge out of `main`'s frame. Clone the one it consults into the
    // closure; `Judge` is `Clone` for exactly this, and a clone is a name plus a kind, not a run.
    let tests_for_shrink = tests.clone();
    let no_shrink = Judge::native("no_shrink", move |c| {
        // `value()` is Option<f64> because a BINARY judge emits no number, and conflating "no
        // number" with 0 is a bug agg has already had once. None here means `tests_pass` was not
        // consulted on this step — not that the suite shrank to nothing.
        let Some(now) = c.value(&tests_for_shrink) else {
            return Verdict::binary(true).with_rationale("tests_pass not consulted this step");
        };
        // `previous()` is the last MERGED verdict — the right baseline, because a rolled-back
        // session's count is not a count anything has to beat. It reads `landed_met`'s data
        // (`gate.rs:63`), so on this path "previous" means AS OF THE LAST `agg.gate()`, not as of
        // the last step: nothing merges between gates, so there is no finer baseline to have.
        let prev = c.previous(&tests_for_shrink).and_then(|v| v.value()).unwrap_or(now);
        // Tolerance zero on purpose. Legitimately removing a test is rare enough that it should
        // cost a human an explicit `agg send inject`, not pass silently.
        // `with_`-prefixed setters: two inherent methods cannot share the reader's name (E0592,
        // corrected 2026-08-04 — RUST_API §7.5).
        Verdict::binary(now >= prev).with_value(now).with_rationale(if now >= prev {
            format!("{now} tests (was {prev})")
        } else {
            format!("session {}: TEST COUNT FELL {prev} → {now} — a test was deleted or disabled",
                    c.session())
        })
    });

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // THE FLOW
    // ═════════════════════════════════════════════════════════════════════════════════════════

    // Declared once (`pos`) and iterated once (`for`) — one number, from the caller.
    let cycle = agg.pos("cycle", cycles as u64);

    'cycle: for c in 0..cycles {
        cycle.update(c as u64);

        // THE CEILINGS, ENFORCED. `.limits(..)` above is a budget; this is what makes it a ceiling.
        // Once per generation is right for this loop — a generation is long, and stopping halfway
        // through one saves nothing worth the complexity of checking more often.
        agg.check_limits()?;

        // ── SELECT + DESIGN — do not build against a vague contract ────────────────────────
        agg.step(&select)?;
        for _ in 0..3 {
            agg.step(&design)?;
            // `value_or(d)`, not `value()`: the reader is `Option<f64>` (a binary judge emits no
            // number — see `no_shrink` below, which depends on exactly that), so a comparison needs
            // the driver to STATE its default rather than have agg fabricate a 0.
            if agg.judge(&design_sound).value_or(0.0) >= 85.0 {
                break;
            }
            agg.info("design still vague — redrafting");
        }
        // Re-testing is free (memoized for this step), so there is no `drafted_ok` bool and no
        // `Status::Exhausted` to match on. Three vague drafts means the ROADMAP item was
        // underspecified, so the right move is a different item — not implementing from a fog.
        if agg.judge(&design_sound).value_or(0.0) < 85.0 {
            agg.info("3 drafts and still vague — picking a different roadmap item next cycle");
            continue 'cycle;
        }

        // ── BUILD — first cut, then grind to green ─────────────────────────────────────────
        let r = agg.step(&implement)?;
        agg.log(&format!("implement: {} tokens, landed={:?}", r.tokens, r.landed));   // Debug: no Display on `Landing`
        if !agg.judge(&builds).met() {
            agg.info("first cut does not compile — starting over next cycle");
            continue 'cycle;
        }

        // STALL DETECTION, IN THE DRIVER. Two laps that fix nothing means the same vendor is going
        // in circles, so the next lap goes to a different one. This is what the `stalled` judge used
        // to do, and here it is three lines of Rust that (a) can change WITHIN the loop, which a
        // verdict-file-backed judge cannot — rows land per `gate()`, not per step (OD-26) — (b)
        // never reaches `gate()`'s regression set, so it cannot be mistaken for a goal, and (c) is
        // replay-safe: on `--resume` the memoized verdicts feed the same comparison and the loop
        // takes the same branches.
        let mut flat_laps = 0;
        let mut last_green = 0;
        for _ in 0..15 {
            // A different vendor gets the step-back; the same one gets the routine fix.
            if flat_laps >= 2 {
                agg.step(&reconsider)?;
                flat_laps = 0;
            } else {
                agg.step(&repair)?;
            }
            let green = [&builds, &lint, &tests].iter().filter(|j| agg.judge(j).met()).count();
            if green == 3 {
                break;
            }
            // no forward movement this lap — count it
            if green <= last_green { flat_laps += 1; } else { flat_laps = 0; }
            last_green = green;
        }
        if !(agg.judge(&tests).met() && agg.judge(&lint).met()) {
            // The work is NOT lost by leaving without a gate: every session is committed on its own
            // branch and session N+1 branches off session N, so the next cycle's worker still opens
            // the partial implementation. What has not happened is a merge to base.
            agg.ask(&format!("cycle {c} did not converge in 15 laps — different approach?"));
            continue 'cycle;
        }
        if !agg.judge(&no_shrink).met() {
            agg.ask(agg.judge(&no_shrink).rationale());   // memoized: the second ask is free
            continue 'cycle;
        }

        // ── VERIFY — the centrepiece ───────────────────────────────────────────────────────
        // Four judges, `&&`, one line. The three cheap ones were already consulted this step so
        // re-listing them costs nothing, and `&&` short-circuits before `e2e` unless all three are
        // green. Across a 60-step run that is ~20 minutes of e2e instead of ~10 hours. There is no
        // `gate:` field, no `when:` on the judge and no run-set to declare — the flow already says
        // it, in the language the reader already knows.
        if !(agg.judge(&builds).met()
            && agg.judge(&lint).met()
            && agg.judge(&tests).met()
            && agg.judge(&e2e).met())
        {
            agg.info("e2e red after a green cheap ladder — not landable this cycle");
            continue 'cycle;
        }

        // Every `step()` above only STAGED — work committed on the session branch, nothing merged.
        // `gate()` closes the span and hands agg the keep/rollback call over every verdict consulted
        // since the last gate, under the `on_regression` set in `main`. The driver says WHEN, the
        // policy says WHAT — an `agg.rollback()` was proposed and rejected for inverting exactly
        // that. Note this is unrelated to the deleted judge-level `gate:` field above.
        //
        // ⚠ AT THIS GATE THE POLICY IS A BACKSTOP, NOT THE MECHANISM. The `&&` ladder above already
        // refused to reach here with any judge unmet, so this span's regression set is all-met by
        // construction and `Rollback` cannot fire. That is not a defect — the ladder is the STRICTER
        // rule (it blocks red work whether or not it ever landed green). `Rollback` earns its keep at
        // the SECOND gate below, where the span is graded after the work instead of before it.
        //
        // THE REGRESSION SET IS SIMPLY WHAT THIS DRIVER ASKED: `builds`, `lint`, `tests`, `e2e`,
        // `no_shrink` — every one of them met = GOOD, so "was met, now unmet" always means "worse".
        // No exclusion rule and no marker are needed, because there is no inverted detector in the
        // span: the stall check above is a local variable, not a judge (see the block where the
        // `stalled` judge used to be). That is the whole reason it is a local variable.
        agg.gate()?;

        // ── LAND — merge and tag. NOT restart. ─────────────────────────────────────────────
        // The new judge grades the NEXT run, never this one: this driver is already compiled.
        agg.step(&author_judge)?;

        // The one human gate, and the only one. Landing is autonomous; deciding that a generation
        // is the one the next run picks up is not. `block` is USER-driven — the worker can never
        // reach it — and ceilings keep firing while it waits, so `wall_hours` ends the run rather
        // than hanging until morning.
        agg.block("generation is green end-to-end. Tag it as the next agg?")?;

        agg.step(&land)?;

        // GRADE THE SECOND SPAN BEFORE IT LANDS. This span writes a NEW JUDGE and the tag the next
        // generation boots from — the one merge in the whole driver that must never go in unverified.
        // Asking here is what puts any verdict in the span AT ALL: with none, `gate()`'s regression
        // set is empty, `verdicts::append` returns early on the empty slice (no `verdicts.jsonl` row
        // is written), and the span merges silently ungraded. It is also the only place
        // `OnRegression::Rollback` can fire in this driver — `builds`/`tests` were landed-met one
        // gate ago, so a generation that breaks its own build is DISCARDED here rather than tagged
        // as the next agg. The policy does the discarding; this `if` only names the reason.
        if !(agg.judge(&builds).met() && agg.judge(&tests).met()) {
            agg.info("authoring the judge or tagging broke the tree — this generation is not landable");
        }
        agg.gate()?;                       // second span: author_judge + land, same policy
        agg.log(&format!(
            "generation {c} landed. The NEXT `agg run` uses it; this process does not."
        ));
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// THE RECURSIVE HAZARDS, AND WHAT ACTUALLY DEFENDS AGAINST EACH
//
// Only defences enforced by code at cc30d43 or by a decided design are listed. Everything in the
// "sandbox" column is unenforced under `isolation: none`, which is STILL THE DEFAULT.
//
//   HAZARD                          DEFENCE                                       BINDS
//   ─────────────────────────────── ───────────────────────────────────────────── ──────────────────
//   delete the failing test         `repair` inherits `readonly(["tests/"])` and   sandbox only
//                                   re-grants nothing — a deny, not a prompt       (design, §4b.5)
//   disable a test from inside a    `no_shrink`: the count must not fall. A        always — but it
//   file it may legitimately write  detector, cleared by a human `agg send inject` detects, not bars
//   suborn a script judge into      `agg/judges/` denied by default under sandbox; sandbox only
//   met:true                        only `author_judge` re-grants it               (design, §4b.5)
//   forge verdicts.jsonl to fake    `agg/private/` carve-out, derived from cwd in  sandbox|container
//   progress — or a stall           `wrap()` (isolation/mod.rs:151), no knob.      — SHIPPED cc30d43
//                                   Proven by `isolation::tests::private_dir_
//                                   is_carved_out_of_the_writable_cwd`, mutated
//   `agg send budget` to raise its  `bus/` lives under `agg/private/`              as above, shipped
//   own token ceiling               (paths.rs:135) — the same carve-out
//   weaken src/isolation/, escape   THE GENERATION BOUNDARY: this run's            structural, always
//                                   confinement was compiled before the change
//   break the gate or the judges    the generation boundary again                  structural, always
//   scope creep across 200 files    NOTHING in code. The `select` prompt asks for  unenforced — said
//                                   one-run items; the rest is review at the tag   so it is not
//                                                                                  mistaken for one
//
// THE HONEST GAP: under `isolation: none` every "sandbox only" row above is unenforced — the worker
// has the whole filesystem and no directory layout changes that. Running THIS workflow without
// `isolation: sandbox` means trusting the worker not to touch the machinery grading it. Under
// `none` the moat is a protocol boundary, not a permission one (HANDOFF §5).
//
// THE RESIDUAL WITH SANDBOX: `agg/judges/` is committed by design, so that a rollback can restore a
// tampered grader — and `author_judge` deliberately punches a hole in the deny for exactly one
// step. That step is the narrowest form of the problem that still lets a self-improving loop write
// its own graders; it is not zero. `no_shrink` is not an answer to it (a native judge is not
// safer, it is just faster) — the answer is that the hole is one named step wide and the diff is
// reviewed at the tag.
//
// AND IF THE DRIVER NEVER REACHES `gate()` — a run where no cycle ever gets past e2e — nothing is
// merged and nothing is discarded: the sessions sit on the span branch, base is untouched, and agg
// says so at run end with the one `git merge` that would land them. Neither silent outcome (a
// dropped overnight run, a moved `main` nobody approved) can happen.
// ─────────────────────────────────────────────────────────────────────────────────────────────
