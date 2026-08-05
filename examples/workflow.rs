//! SAMPLE — a multi-stage workflow in the Rust API.
//!
//! ✅ **THIS COMPILES.** It is a cargo example (`cargo build --example workflow`), which is what
//! keeps it honest: it was the SPECIFICATION of the surface while the surface was being built, and
//! now it is the acceptance test for it. It is not meant to be RUN as-is — it names judge files and
//! a project that do not exist here — but every call, every type and every signature below is the
//! real one, checked by the compiler on every build.
//!
//! REVISION 8 (2026-08-05): compiled for the first time against the shipped API. Two things changed
//! and both are noted where they happen — `MINUTES` was never a real constant (`.timeout()` takes
//! seconds), and nothing else. Rev 7 stands.
//!
//! REVISION 7 (2026-08-04). Rev 6 was the owner line-by-line review (one `Agent` enum — naked LLMs
//! cut — all config in Rust, `agg.gate()`); rev 7 applies the two audit rounds and the owner's YAML
//! simplification: `with_*` Verdict setters (two methods cannot share a name — E0592),
//! `GateOutcome::Failed`, no judge-level `gate:`/`when:` ANYWHERE, `Opts { resume }` as the
//! fast-forward opt-in. Six ideas; everything else follows.
//!
//!   1. NO NODE MODEL. Flow is `for` / `if` / `break` / labeled `continue`. No Stage, Sequence,
//!      Action, Loop, guard or if_else — reinventing Rust's control flow as data was backwards.
//!   2. `agg.step(&s)?` RETURNS the step's outcome. `agg.judge(&j)` returns its verdict.
//!   3. JUDGES ARE LAZY and memoized per step, so `&&` short-circuits into gating for free — and
//!      re-testing a condition costs nothing, which is what kills accumulator bools.
//!   4. `&Agg`, not `&mut Agg`. Laziness means reads mutate a cache; interior mutability keeps that
//!      out of the signature so `mut` never appears in user code.
//!   5. THREE notification levels, because "tell me" and "I am stuck without you" are different.
//!   6. `step()` ALWAYS STAGES; `agg.gate()` closes the span. The driver says WHEN a keep/rollback
//!      decision happens, the `on_regression` policy says WHAT it decides. A gate inside `step()`
//!      could not work: lazy judges are consulted AFTER the step, so it had nothing to compare.
//!
//! The API is: `Agg::open` · `agg.step()` · `agg.judge()` · `agg.gate()` · `agg.info/ask/block()`
//! · `agg.pos()`.
//!
//! NARRATIVE: add rate limiting to a public HTTP API. Nobody has decided how yet.
//!   discover → design → build → harden → ship, up to 20 cycles.

use agg::prelude::*;

/// `main` is the ENTRY POINT ONLY: it reads the two knobs a run needs from outside (where, and how
/// many cycles), opens the project, and hands off. Everything that IS the sample — steps, judges,
/// the flow — lives in [`drive`], so a harness can drive this exact driver against a scratch project
/// (`scripts/samples_real.sh`) without a fork of it existing to drift from this one.
///
///   ./workflow                      # cwd, 20 cycles — today's values, unchanged
///   ./workflow /tmp/proj 2          # argv:  <dir> [cycles]
///   AGG_SAMPLE_DIR=… AGG_SAMPLE_CYCLES=2 ./workflow      # or env, for a launcher that has no argv
fn main() -> Result<(), Fatal> {
    let dir = arg(1, "AGG_SAMPLE_DIR").unwrap_or_else(|| ".".to_string());
    let cycles = arg(2, "AGG_SAMPLE_CYCLES").and_then(|s| s.parse().ok()).unwrap_or(20);

    // NO CONFIG FILE. A Rust driver configures EVERYTHING in Rust; a stray `agg.yaml` is ignored.
    // The two paths are independent — half of the policy in YAML and half in code is the split-brain
    // this design spent months avoiding. What the paths share is FILES, not config: `agg/judges/*`,
    // `agg/AGG.md`, `agg/state/`.
    let agg = Agg::open(dir)?              // no `mut` — see idea 4
        // A BUDGET, NOT A TRIPWIRE. `.limits()` records the ceilings; `agg.check_limits()?` in the
        // flow is what enforces them. Nothing fires on its own — see the call in the cycle below.
        // (`wall_hours` is a `Limits` field on the Rust path; in YAML it is a condition term.)
        .limits(Limits { tokens: Some(40_000_000), cost: None,
                         sessions: Some(400), wall_hours: Some(12.0) })
        // What a regression MEANS, applied by `gate()`. `Annotate` (the default) always merges and
        // tells the next session what regressed plus the SHA to revert to; `Rollback` discards the
        // span. The driver says WHEN to gate; this says WHAT a failure means.
        .on_regression(OnRegression::Annotate);

    drive(&agg, cycles)
}

/// One positional arg, or an env var, or nothing. The env fallback exists because the process that
/// launches an unattended run is often a script that already has the values in its environment.
fn arg(n: usize, var: &str) -> Option<String> {
    std::env::args().nth(n).or_else(|| std::env::var(var).ok()).filter(|s| !s.is_empty())
}

/// A model name a RUN may need to pin. The default is the one the narrative names; the override is
/// how `scripts/samples_real.sh` points a real session at a model that exists on the day it runs,
/// without editing (and so drifting) the driver. Nothing else about the step changes.
fn model(default: &str, var: &str) -> String {
    std::env::var(var).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

/// THE SAMPLE. `&Agg` (idea 4) and a cycle bound — the two things a driver needs and the only two
/// things the caller decides. Read from here down as if it were `main`; it was, until the split.
fn drive(agg: &Agg, cycles: usize) -> Result<(), Fatal> {
    // ⛔ THERE IS NO `.abort_if()` / `.notify_if()` ON THE RUST PATH. They are condition STRINGS
    // over judges, and this file already has judges and `if` — writing the same logic twice, once in
    // Rust and once in a mini-language, is the thing this API exists to delete. Want to stop when a
    // judge says so? `if agg.judge(&x).met() { return Ok(()); }`. Want to page a human? `agg.ask()`.
    // Both are below, in Rust, where the compiler checks them. YAML keeps both keys — it has no `if`.

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // STEPS — builders. Rust has no named or default arguments, so the options were builder,
    // struct literal or macro. The builder keeps the name first, drops `Some`/`.into()`/
    // `..Default::default()`, and keeps IDE completion and real error messages.
    // `Step` is a serde struct underneath for every field YAML can express — that is what "one
    // struct, two constructors" means. The documented exception is `Agent::Custom(Arc<dyn ..>)`,
    // which cannot be `Deserialize`: the Rust builder is a superset, not a mirror.
    // ═════════════════════════════════════════════════════════════════════════════════════════

    // TEMPLATES. `Step::template()` is an unnamed Step; `.create(name)` clones it into a real,
    // named step. This is YAML's `defaults:` block — except YAML has exactly ONE and Rust can have
    // as many families as the workflow has kinds of step.
    //
    // Merge rules: scalar fields (agent/model/effort/isolation/prompt) OVERRIDE; `readonly`
    // ACCUMULATES, so a derived step cannot silently lose a protection the template set; and
    // `writable` subtracts from what accumulated. That asymmetry is what makes `implement` below
    // readable — it re-grants exactly one deny instead of re-listing the ones it still wants.

    // There is ONE agent enum — `Claude | Codex | Copilot | Custom(..)`, all coding harnesses.
    // Naked LLMs are cut: without tools a model cannot open a file or explore a repo, so it only
    // ever covered fixed-input steps — not worth an HTTP client plus an async runtime in a codebase
    // that has neither.
    // ⚠ `.readonly()` IS A NO-OP WITHOUT A CONFINING TIER, so `reading` sets one. The deny list is
    // delivered by the OS wrapper, and the wrapper only runs under `sandbox`/`container` — under the
    // default `none` there is no mechanism to deliver it to, and the step can write every path
    // listed. agg warns when it sees `readonly`/`writable` on an unconfined step; it does not
    // silently pretend. (An earlier revision of this file omitted the tier here and then claimed the
    // deny was doing the work. It was not.)
    let reading = Step::template()           // reads and writes prose; never touches source
        .agent(Agent::Claude)
        .effort(Effort::High)
        .isolation(Isolation::Sandbox)
        .readonly(["src/", "tests/", "agg/judges/"]);

    let sandboxed = Step::template()         // touches real source
        .agent(Agent::Claude)
        .isolation(Isolation::Sandbox)
        .readonly(["tests/", "agg/judges/"]);

    // Ideation, but it still needs a harness: the survey has to read the existing handler code and
    // write a file. What makes it cheap is the deny list, not a weaker agent.
    let survey = reading.create("survey")
        .prompt("Survey rate-limiting approaches (token bucket, sliding window, GCRA). \
                 Cite sources. Write the comparison to agg/state/wiki/survey.md.");

    // A DIFFERENT REVIEWER looks at the survey — perspective diversity, which agg buys per step and
    // which a different prompt alone does not give you. Not built from `reading`, because this step
    // overrides the agent-level knobs that template fixes.
    //
    // ⚠ The STRONGEST form of this is a different VENDOR — `.agent(Agent::Codex)`, one word — and it
    // is what this step shipped until 2026-08-05. It is written with a different MODEL here so the
    // sample runs for a reader who has only one agent installed; a run that has both should put the
    // vendor back. What is being demonstrated either way is that the knob is PER STEP.
    //
    // Whichever agent this is, it is confined by the SAME OS wrapper as every other one. agg does not
    // delegate its moat to an agent's own sandbox — an agent vouching for itself is not a guarantee —
    // so the wrapper is applied unconditionally.
    //
    // ⚠ There is exactly ONE layer, and it is agg's. Kernel sandboxes do not NEST: Seatbelt permits
    // a second `sandbox_apply` only from a process whose current profile is entirely unrestricted,
    // so agg's wrapper plus Codex's own `-c sandbox_mode=workspace-write` gives
    // `sandbox_apply: Operation not permitted` and the worker dies at launch. agg therefore disables
    // an agent's NATIVE sandbox on every tier — that flag disables the AGENT's confinement, never
    // agg's, and on the `sandbox` tier the process still runs inside agg's kernel jail. The moat was
    // never the agent's to hold.
    let spec = Step::new("spec")
        .model(model("claude-sonnet-5", "AGG_SAMPLE_REVIEW_MODEL"))
        .isolation(Isolation::Sandbox)
        .prompt("Turn agg/state/wiki/survey.md into an implementable spec at \
                 agg/state/wiki/spec.md. Assume the survey is incomplete; say what it missed.");

    let implement = sandboxed.create("implement")
        // a String: new models arrive weekly; an enum would need an agg release for each one — and
        // for the same reason a RUN can pin one (`AGG_SAMPLE_HEAVY_MODEL`), see `model()` above.
        .model(model("claude-opus-4-8[1m]", "AGG_SAMPLE_HEAVY_MODEL"))
        .effort(Effort::High)
        // This step SHOULD add tests, so it re-grants that one deny. `agg/judges/` stays denied —
        // which is the payoff of `writable` subtracting rather than replacing.
        .writable(["tests/"])
        .prompt("Implement agg/state/wiki/spec.md. Add tests alongside.");

    // Inherits `readonly(["tests/", "agg/judges/"])` and does NOT re-grant it. The obvious way to
    // make a failing test pass is to delete it, and `fix` has no legitimate reason to touch
    // `tests/` — enforced by the kernel, not by the prompt below.
    let fix = sandboxed.create("fix")
        .model(model("claude-sonnet-5", "AGG_SAMPLE_GRIND_MODEL"))   // a cheaper model for the grind
        .prompt("Make the failing tests and lints pass. Do not delete tests.");

    let harden = sandboxed.create("harden")
        .prompt("Review the rate limiter for abuse: clock skew, distributed counters, \
                 burst handling. Fix what you find.");

    let document = Step::new("document")   // no template: writes docs, touches no source
        .prompt("Document the rate limiter in docs/ and write a release note.");

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // JUDGES — three kinds, all lazy. Nothing is registered and nothing runs here.
    //
    // A judge returns a VERDICT, not a number: `met: bool` + `value: Option<f64>` + max + target +
    // rationale + evidence. `value` is optional because a BINARY judge emits no number, and
    // conflating "no number" with `0` is a bug agg has already had once.
    // ═════════════════════════════════════════════════════════════════════════════════════════

    // RUBRIC — the .md file IS the prompt. Graded by the ruler; costs a model call.
    let survey_good = Judge::rubric("survey_good", "agg/judges/survey_good.md");
    let spec_sound  = Judge::rubric("spec_sound",  "agg/judges/spec_sound.md");

    // SCRIPT — any executable. Cheap, deterministic, no model call.
    let builds = Judge::script("builds",     "agg/judges/build.sh");
    let lint   = Judge::script("lint_clean", "agg/judges/lint.sh");
    let tests  = Judge::script("tests_pass", "agg/judges/tests.sh");

    // 40 minutes of synthetic load. No `gate` field: laziness plus `&&` in the flow is what stops
    // it running on the ~55 steps where the build is still red.
    let load = Judge::script("load_ok", "agg/judges/loadtest.sh")
        // SECONDS. Earlier revisions of this file wrote `45 * MINUTES`; there is no such constant,
        // and adding one to the prelude to serve two call sites is a worse trade than `45 * 60`.
        .timeout(45 * 60);            // the run-level 300s default would kill it

    // NATIVE — a Rust closure over a `JudgeCtx`. Reach for it when a check is genuinely easier or
    // faster in Rust than in a script: no subprocess, no fork per judge, and the compiler checks it.
    //
    // `JudgeCtx` offers `met/value/verdict/previous/history` over other judges, plus `session()`,
    // `step()`, `dir()`, `read()` and `diff()`. It offers NO clock, NO randomness, NO network and NO
    // env — deliberately. A judge that reads the wall clock returns a different verdict on replay,
    // and `--resume` fast-forward would then silently diverge from the run it claims to reproduce.
    let p99 = Judge::native("p99_ok", |c| {
        // `c.scratch()` — the per-session directory judges may write, SHARED between them. That
        // sharing is the point here: `load_ok` above measures and writes `bench.json`, this judge
        // applies the threshold. It cannot be `agg/state/bench.json` any more, because since §2.5 the
        // project tree is read-only to a judge — a judge able to write the tree it grades can make
        // the code pass, which is the same hole as letting one declare its own writable set.
        let ms = std::fs::read_to_string(c.scratch().join("bench.json")).ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["p99_ms"].as_f64())
            .unwrap_or(f64::MAX);
        // `Verdict::binary(met)` is the constructor twin of the shipped `Verdict::failed(err)`
        // (`model.rs:69`); `.with_value()`/`.with_rationale()` are setters on the way out — `with_`
        // prefixed because two inherent methods cannot share the reader's name (Rust has no method
        // overloading; E0592). Corrected in the 2026-08-04 final audit — see RUST_API §7.5.
        Verdict::binary(ms <= 5.0).with_value(ms).with_rationale(format!("p99 {ms}ms"))
    });

    // ═════════════════════════════════════════════════════════════════════════════════════════
    // THE FLOW — ordinary Rust.
    // ═════════════════════════════════════════════════════════════════════════════════════════

    // `pos` is the one thing agg needs from a hand-written loop: it cannot see `for`, so this is
    // how "cycle 7/20" reaches the TUI. Its Drop pops the frame on every exit path.
    // The bound is DECLARED once (`pos`) and iterated once (`for`) — one number, from the caller.
    let cycle = agg.pos("cycle", cycles as u64);

    'cycle: for c in 0..cycles {
        cycle.update(c as u64);

        // THE CEILINGS, ENFORCED — once per cycle, where THIS driver wants them enforced. agg does
        // not stop the run behind your back: `.limits(..)` above is a budget, and this is the check.
        // A breach returns `Err(Fatal::Ended(..))` and `?` unwinds out of `main`. Never called ⇒
        // never enforced; that is the deal, and it is the same deal `?` makes everywhere else here.
        // Cheap (it reads counters agg already keeps), so call it wherever a long stretch begins —
        // top of the cycle is the obvious place. It also lets agg see `agg stop` and Ctrl-C.
        agg.check_limits()?;

        // ── DISCOVER — up to 3 attempts at a survey worth building on ───────────────────────
        for _ in 0..3 {
            // The `?` is not about the outcome — that is discarded here. It is the error channel: a
            // hard pipeline failure (a broken worker) and, once `check_limits()` has latched, the
            // ceiling. Omitting it is a compiler warning, not silence — `Result` is `#[must_use]`.
            agg.step(&survey)?;
            // `value()` is `Option<f64>` — a BINARY judge emits no number and flattening that to 0
            // is a bug agg has already shipped once. `value_or(d)` is the ONE helper that turns it
            // into an `f64`, and it makes the default the DRIVER's stated choice rather than a
            // fabrication. Here 0.0 is right: a rubric that produced no score is not a good survey.
            if agg.judge(&survey_good).value_or(0.0) >= 85.0 {
                break;
            }
            agg.info("survey still thin — trying a different angle");
        }
        // Re-testing is FREE — the verdict is memoized for this step — so there is no accumulator
        // bool and no `Status::Exhausted` variant. Just ask again.
        if agg.judge(&survey_good).value_or(0.0) < 85.0 {
            agg.ask("3 surveys and still thin. Any prior art you want me to start from?");
            continue 'cycle;
        }

        // ── DESIGN ─────────────────────────────────────────────────────────────────────────
        agg.step(&spec)?;
        let v = agg.judge(&spec_sound);
        if !v.met() {
            agg.info(&format!("spec rejected: {}", v.rationale()));
            continue 'cycle;
        }

        // ── BUILD — first cut, then grind to green ─────────────────────────────────────────
        // The step outcome is worth reading: it carries what the session cost and where the work
        // ended up. On the Rust path `landed` is normally `Span` — `step()` always stages, so `Base`
        // is what a `gate()` produces, not a step — and `Nothing` means the session committed
        // nothing at all (empty or vetoed). That distinction is why `Landing` is an enum, not a bool.
        let r = agg.step(&implement)?;
        // `{:?}`, not `{}` — `Landing` is a plain data enum (Debug/Clone/Copy/Serialize, §12.3) and
        // deliberately implements no `Display`: how a landing is PHRASED is a UI decision, and the
        // dashboard, the ledger and a driver's log all want different words for `Landing::Span`.
        agg.log(&format!("implement: {} tokens, landed={:?}", r.tokens, r.landed));

        if !agg.judge(&builds).met() {
            agg.info("implementation does not compile — starting over next cycle");
            continue 'cycle;
        }
        for _ in 0..8 {
            agg.step(&fix)?;
            if agg.judge(&tests).met() && agg.judge(&lint).met() {
                break;
            }
        }
        if !agg.judge(&tests).met() {
            agg.ask(&format!("build did not converge in cycle {c} — worth a different approach?"));
            continue 'cycle;
        }

        // Every `step()` above only STAGED: the work is committed on its session branch and nothing
        // has merged. `gate()` closes that span and lets agg decide keep-or-rollback over ALL
        // verdicts consulted since the last gate, under the `on_regression` policy set in `main`.
        // The DRIVER says WHEN, the POLICY says WHAT — which is why there is no `agg.rollback()`:
        // that would hoist a policy decision to the call site and make agg's git handling manual.
        //
        // ⚠ "GATE" USED TO MEAN TWO THINGS. Since 2026-08-04 it means only this one: the judge-level
        // `gate:` field was cut from YAML as well (owner simplification — all judges run after every
        // step there), so `agg.gate()` is the only gate in the design. Its question is "does the
        // staged work LAND?", never "does this judge run?" — that one is `&&`, used just below.
        //
        // The outcome is worth matching on when the driver has something to say about it. `Failed`
        // is NOT `RolledBack`: a merge conflict is not a policy decision, the branch is still there,
        // and the work is still gateable once a human resolves it.
        // (`if let`, not `match` — the other three outcomes mean "agg applied the policy", and there
        //  is nothing for the driver to add to Kept, RolledBack or Nothing.)
        if let GateOutcome::Failed(why) = agg.gate()? {
            agg.ask(&format!("cycle {c}: the span would not merge ({why:?}) — resolve and re-run?"));
            continue 'cycle;
        }

        // ── HARDEN — and here is the gating, for free ──────────────────────────────────────
        // `&&` short-circuits, so `load` (40 minutes) is only reached on a cycle where the build is
        // already green. Over 20 cycles that is ~40 minutes of load testing instead of ~40 hours.
        // This is what a judge's `gate:` FIELD was for — and since that field is now cut from YAML
        // too, this short-circuit is the ONLY judge-gating anywhere in the design. It is also the
        // sharpest reason to reach for the Rust path: a YAML project runs `load` after every step.
        agg.step(&harden)?;
        if !(agg.judge(&tests).met() && agg.judge(&load).met() && agg.judge(&p99).met()) {
            agg.ask("performance gate failed — ship anyway, or keep tuning?");
            continue 'cycle;
        }

        // ── SHIP ───────────────────────────────────────────────────────────────────────────
        // The one place this loop genuinely cannot proceed alone. `block` drains the operator bus
        // and waits — and ceilings keep running while it does, so `wall_hours` ends the run rather
        // than hanging forever. It is opt-in and explicit; nothing else here blocks.
        agg.block("rate limiter is green and ready to tag. Approve the release?")?;

        agg.step(&document)?;
        agg.gate()?;                       // second span: harden + document, same policy
        // `?` on an io::Error inside `fn main() -> Result<(), Fatal>` works because `Fatal` carries
        // `From<std::io::Error>` (RUST_API §4.1). And this write is IDEMPOTENT on purpose: side
        // effects outside `agg.*` re-execute on a fast-forward replay.
        std::fs::write("agg/state/RELEASE_NOTES.md", agg.summary())?;
        agg.log("shipped");
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// THE WHOLE API
//   Agg::open(dir).limits(..).on_regression(..).instructions(..) -> Agg
//                                            (takes &self everywhere after the builder chain)
//   agg.check_limits()                    -> Err(Fatal::Ended) if a ceiling is breached; opt-in
//   agg.step(&step)                       -> StepOutcome    { step, session, landed, verdicts,
//                                                             tokens, cost, secs, exit }
//   agg.judge(&judge)                     -> Verdict        { met(), value(), value_or(d), max(),
//                                                             rationale() }
//   agg.gate()                            -> GateOutcome    { Kept | RolledBack | Nothing |
//                                                             Failed(GateFailure) }
//   agg.info(msg) / ask(msg) / block(msg) -> the three notification levels
//   agg.pos(label, max) / .update(i)      -> the breadcrumb for a hand-written loop
//   agg.log(msg) / agg.summary()
//   Step::new(name) / Step::template().create(name)
//        .agent().model().effort().isolation().readonly().writable().prompt()
//   Judge::rubric/script/native(..).timeout()      // no `.when()` — cut 2026-08-04 with YAML's
//   Verdict::binary(met).with_value(n).with_rationale(s) — CONSTRUCTION, for a native judge's return
//
// ⚠ READER vs SETTER (corrected 2026-08-04): the reader is `value() -> Option<f64>` (a binary judge
//   emits no number; a fabricated `0` is a bug agg has shipped once); the setters carry a `with_`
//   prefix because two inherent methods CANNOT share a name in Rust (no overloading — E0592).
//   Comparisons go through `value_or(d)` so the default is the driver's stated choice.
//
// PATH SPELLING IN `readonly`/`writable`: write directories with a TRAILING SLASH and spell them
//   identically in both lists. `writable` SUBTRACTS from what `readonly` accumulated, so
//   `writable(["agg/judges"])` against `readonly(["agg/judges/"])` subtracts nothing and the deny
//   silently survives. agg normalises before comparing — but a sample that relies on that is a
//   sample that teaches the reader to.
//
// THE THREE NOTIFICATION LEVELS
//   info   FYI. Lands in the log and the reader. Nothing is expected back.
//   ask    A response would help; the loop CONTINUES regardless. Picked up at the next session
//          boundary if one arrives. This is `notify_if`'s non-terminal contract, made explicit.
//   block  Cannot proceed without a human. Waits on the operator bus. OPT-IN and rare — the whole
//          point of agg is that this is a deliberate choice, never the mechanism.
//
// THE WRITABLE SET, three levels
//   ALWAYS DENIED, derived from cwd, NOT configurable: `agg/private/` — agg's own ledger, bus and
//     pidfile. Derived inside `wrap()` so no call site can forget it; there is no knob to disable it.
//   DENIED BY DEFAULT under `sandbox`, re-grantable: `agg/judges/`. A step that legitimately authors
//     judges says `.writable(["agg/judges/"])`; no other step can reach them.
//   PER-STEP, opt-in: `.readonly([..])` adds denies (see the `sandboxed` template), `.writable([..])`
//     re-grants (see `implement`). readonly ACCUMULATES down a template chain; writable subtracts.
//   Same mechanism as the private carve-out — Seatbelt `(deny file-write* (subpath ..))`, bwrap
//   `--ro-bind`, docker `:ro` — just parameterised. Under `isolation: none` NONE of it binds.
//
// NOT INVISIBLE, AND THAT IS THE POINT
//   ceilings        `.limits(..)` declares them; `agg.check_limits()?` at the top of the cycle
//                   enforces them. agg never ends the run behind the driver's back. Once a check
//                   latches, every later call is a no-op and the next `?` carries you out.
//                   ⚠ A driver that never calls `check_limits()` has no ceilings. Deliberate: the
//                   driver is code you compiled, not the worker. The moat that is NOT optional is
//                   the worker's — `agg/private/` is carved out of its writable set, so it cannot
//                   forge a verdict or raise its own budget no matter what it writes.
//   done_if         YAML-only. On the Rust path the driver decides it is done by returning.
//   fast-forward    OPT-IN, via `Agg::open_with(".", Opts { resume: true })` — agg does not own the
//                   driver's argv, so a driver forwards its own `--resume` flag there. Completed
//                   `agg.step()`/`judge()`/`gate()` calls then replay from the ledger at zero cost.
//                   This is why a native judge gets no clock and no RNG — see `p99` above.
//   on_regression   `.on_regression(..)`, also in Rust. `Annotate` (the default): a regressing
//                   session still merges, and the next brief carries what regressed plus the commit
//                   to revert to. Applied by `gate()`, never inside `step()`.
//
// IF YOU NEVER CALL `gate()` — nothing is lost and nothing moves: every session is committed on its
//   own branch and the span just keeps growing, so base never advances. agg neither auto-merges (it
//   was not given that call) nor auto-rolls-back (discarding an overnight run over one late
//   regression is worse than keeping it) — it prints, at run end, the span tip and the one
//   `git merge` that lands it.
//
// TWO CONSEQUENCES OF LAZINESS, worth knowing
//   * The scoreboard shows only the judges a step actually consulted. Honest (nothing ran, so
//     nothing to report) but the dashboard's judge list varies between steps on the Rust path.
//   * `&Agg` works because the verdict cache lives behind interior mutability. The trade is that a
//     borrow conflict becomes a runtime panic rather than a compile error — fine single-threaded,
//     and the alternative is `mut` on something that reads immutable.
// ─────────────────────────────────────────────────────────────────────────────────────────────
