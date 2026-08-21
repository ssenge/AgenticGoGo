# The Rust driver API

**This is heavier and Rust-only — use `agg.yaml` unless you need flow it cannot express.**

The YAML path is a *list of steps that laps forever* until `done_if` fires. That covers most of what
people build. The Rust API exists for the runs where the shape of the work is a program: a branch
that depends on a judge's number, a retry ladder whose bound changes with what it learned, a loop
that asks an expensive judge only when three cheap ones already passed.

Everything below is the same engine. `agg.step()` dispatches through the identical primitive the
YAML walk does — one execution model, two ways to drive it. What you gain is `if`, `for`, `?` and
the borrow checker. What you pay is a compiled binary instead of a config file.

---

## When the YAML path is the right answer

Reach for YAML when the flow is a **sequence with bounded repetition**:

```yaml
sequence:
  steps:
    - { step: survey,    until: survey_good.value >= 85, max: 3 }
    - { step: implement }
    - { step: fix,       until: tests_pass AND lint_clean, max: 8 }
  done_if: tests_pass AND lint_clean
```

That is a real loop with real convergence conditions, and it needs no compiler. If your flow fits
this shape, stop here — `examples/workflow.yaml` is a heavily-commented reference.

## When it is not

Four things YAML deliberately cannot do, each of which is a `for`/`if` in Rust:

| you want | YAML | Rust |
|---|---|---|
| skip a step based on a verdict | ✗ — a lap runs every entry (`if:` was cut) | `if agg.judge(&x).met() { continue }` |
| **not run** an expensive judge unless cheap ones pass | ✗ — every run-set judge runs after every judged step | `agg.judge(&cheap).met() && agg.judge(&slow).met()` |
| a bound that changes with what the run learned | ✗ — `max:` is a literal | any expression |
| state that changes *within* a cycle | ✗ — verdict rows land per gate | a local variable |

The second row is the one that costs money. A 40-minute load test in a YAML run-set executes after
**every** judged step; in a driver, `&&` short-circuits and it runs only where it matters.

---

## The surface

Eleven calls. `Agg::open(dir)` then a self-consuming builder chain, then ordinary Rust.

```rust
use agg::driver::{Agg, Judge, Limits, OnRegression, Step, Verdict};

fn main() -> Result<(), agg::driver::Fatal> {
    let agg = Agg::open(".")?
        // SECONDS. `work_time` excludes time blocked on a human — see "Asking a human" below.
        .limits(Limits { tokens: Some(40_000_000), cost: None, sessions: Some(300),
                         wall_time: Some(10.0 * 3600.0), work_time: None })
        .on_regression(OnRegression::Rollback);

    let implement = Step::new("implement").prompt("Make the failing tests pass.");
    let tests     = Judge::script("tests_pass", "agg/judges/tests.sh");
    let slow      = Judge::script("load_ok", "agg/judges/loadtest.sh").timeout(45 * 60);

    let cycle = agg.pos("cycle", 20);          // the breadcrumb readers render
    for c in 1..=20 {
        cycle.update(c);
        agg.check_limits()?;                   // ceilings are OPT-IN — this is where they fire

        agg.step(&implement)?;                 // stages work on a session branch; nothing merged
        if !(agg.judge(&tests).met() && agg.judge(&slow).met()) {
            continue;                          // `&&` IS the cost gate: `slow` never runs when red
        }
        agg.gate()?;                           // close the span: merge it, or apply on_regression
    }
    Ok(())
}
```

| call | what it does |
|---|---|
| `Agg::open(dir)` | opens the project; normalises git; **boots lazily** — nothing is published until real work happens |
| `.limits()` `.on_regression()` `.instructions()` | the builder chain (self-consuming; build judges *above* it) |
| `agg.step(&s)` | run one session. Commits on a session branch — **stages**, never merges |
| `agg.judge(&j)` | this step's verdict, running it if not yet asked. **Memoized per step** |
| `agg.gate()` | close the span: merge everything staged since the last gate, or discard it per policy |
| `agg.check_limits()` | enforce ceilings, drain the operator bus, honour Ctrl-C |
| `agg.pos(label, max)` | an RAII breadcrumb — `cycle 3/20 › attempt 2/3` — published to `state.json` |
| `agg.ask()` `agg.info()` `agg.log()` | the three notification levels — non-blocking, and still the default |
| `agg.hil_bool(q)` `hil_choose(q,&[..])` `hil_input(q)` | **ask a human and get the value back.** BLOCKS until answered |
| `agg.open_asks()` | the asks a human still owes an answer to — including ones the WORKER opened |
| `agg.block(q)` | the old doorbell: waits for `agg send resume`, returns nothing. Prefer `hil_bool` |

### Judges

Three kinds, one type:

```rust
Judge::script("tests_pass", "agg/judges/tests.sh")     // any executable; verdict JSON on stdout
Judge::rubric("spec_sound", "agg/judges/spec.md")      // the .md IS the prompt; graded by the ruler
Judge::native("p99_ok", |c| {                          // a Rust closure over a JudgeCtx
    let ms = std::fs::read_to_string(c.scratch().join("bench.json"))
        .ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["p99_ms"].as_f64()).unwrap_or(f64::MAX);
    Verdict::binary(ms <= 5.0).with_value(ms).with_rationale(format!("p99 {ms}ms"))
})
```

⛔ `JudgeCtx` offers **no clock, no randomness, no network and no environment** — deliberately. A
judge that reads the wall clock returns a different verdict on replay, and resume would silently
diverge from the run it claims to reproduce. A check that genuinely needs the clock is a
`Judge::script`, where the impurity sits in a file a reviewer can see.

`c.scratch()` is the per-session directory judges may write — **shared between them**, so one judge
can measure and another apply a threshold. A judge able to write the tree it grades can make the code
pass, so under a confining tier the project tree is read-only to it and writes are relocated:
`$AGG_JUDGE_SCRATCH` for working files, a persistent per-project dir for toolchain caches
(`CARGO_TARGET_DIR`, `PYTHONPYCACHEPREFIX`, `GOCACHE`, …). Full table in
[docs/CONFIG.md](CONFIG.md#-a-judge-may-not-write-the-project-tree).

⚠ **What confines a driver's judges is `.isolation()` on its STEPS.** There is no separate knob: the
run's tier is the strongest tier any step has declared, and judges take that — never the tier of
whichever step happens to be current, because a judge's confinement follows from its role, not from
its caller.

```rust
let build = Step::new("implement").isolation(Isolation::Sandbox);   // ← this confines the JUDGES too
```

A driver that declares no tier anywhere runs its judges unconfined, exactly as it runs its workers
unconfined. That is the default and it is the honest one — but if you sandbox the worker and not the
judges, the judge script is the way out, because a confined worker can rewrite `agg/judges/*.sh` in
its own writable cwd.

---

## Asking a human

Three calls, one behaviour: **block until a human answers.** No timeout, no default, no ending the
run — an idle process spends no tokens, and waiting is cheaper than the machinery that avoids waiting.

```rust
let i  = agg.hil_choose("Which store?", &["postgres", "sqlite"])?;   // -> usize  (closed set)
let v  = agg.hil_input("Which instance is prod?")?;                  // -> String (open set)
let ok = agg.hil_bool("Deploy to prod?")?;                           // -> bool
```

A human answers with `agg send answer <id> …` (`agg status` lists open asks and their ids). For a
`choose`/`bool` ask the value must be on the recorded list, so the CLI refuses anything else and this
call cannot hand you a value you did not offer — that is why `hil_choose` exists next to `hil_input`.

⛔ **`agg.yaml` has no `hil` key and the worker cannot reach these.** `agg hil` records an ask and
exits; only a driver author, at a call site they wrote, can make the loop wait. With no bound on the
wait, that asymmetry is the only thing standing between this and the interactive agent agg replaces.

Four rules, and they are the whole design:

1. **A human's answer unblocks the STEP; a judge still owns the VERDICT.** Never let a "done" satisfy
   a goal — `while !agg.judge(&dns_ok).met() { agg.hil_bool("created the record?")?; }`.
2. **An answer may NAME a secret, never CONTAIN one.** `hil_input` writes to the ask ledger and the
   next session's brief. Ask for the credential to be *placed*, and confirm with `hil_bool`.
3. **`gate()` before you block for a human who will touch the tree.** `step()` only stages, so a
   human editing an open span collides with staged work. agg warns; it cannot fix it for you.
4. **Set `work_time`, not just `wall_time`.** Ceilings keep firing while blocked, so an overnight
   question would otherwise consume a ceiling meant to measure the agent's effort. `work_time`
   excludes human wait; `wall_time` is a genuine end-to-end deadline.

`agg stop` and Ctrl-C interrupt a wait — the bus is drained on every poll — which is why no timeout is
needed to escape a question nobody will answer.

### Exit codes

`fn main() -> Result<(), Fatal>` collapses **every** ending to exit 1: Rust's `Termination` impl
prints `Error: {:?}` and returns `FAILURE`, so `agg stop`, a blown ceiling and a genuine panic all
look identical to a wrapper. Use `agg::driver::run`:

```rust
fn main() -> std::process::ExitCode { agg::driver::run(real_main) }
fn real_main() -> Result<(), Fatal> { /* … */ }
```

It maps the ending to the same codes `agg run` uses: **0** goals met · **3** `abort_if` · **4**
max-sessions · **5** stopped by an operator · **1** a real fault.

## Resuming a crashed run

An overnight driver that dies at hour six should not start again at hour zero.

```rust
let agg = Agg::open_with(".", Opts { resume: true })?;   // instead of Agg::open(".")
```

Every completed call appended a line to `agg/private/calls.jsonl`, and on resume the same calls are
answered from that file — no worker, no ruler, no git, no tokens. **Your loop is not serialized and
does not need to be:** it runs from the top and its own `if`/`for` walk it back to where it stopped,
because identical inputs produce identical branches.

⚠ **Fast-forward reaches back only as far as the last `gate()` that returned `Kept`.** Everything
after it is discarded and re-executes. That is not conservatism: `step()` always *stages*, so until
a gate lands it the work is on a per-run branch the ledger cannot describe — replaying such a step
as "done" would leave the next session building on an orphaned ref. agg prints exactly what it
drops.

Three consequences worth designing around:

- **Side effects outside `agg.*` re-execute.** A `println!`, a file write or an HTTP POST in your
  loop body happens again during replay. Put once-only effects behind an `agg.*` call (`log`, `info`,
  `ask`, `block` are all recorded) or make them idempotent.
- **An answered `hil_*`/`block()` after the last kept gate will ask again**, and the original answer
  is not recoverable — the one resume cost paid by a person rather than by tokens. Gate before you ask
  if that matters; agg prints exactly which human calls it dropped.
- **Don't branch on the clock, `rand`, or an env var.** Anything derived from a `Verdict` or a
  `StepOutcome` is safe by construction; anything else can send the replay down a different path,
  and agg will refuse rather than guess — a call that does not match the record aborts the resume
  naming both sides.

## Two rules that are easy to get wrong

**A judge's `met` must mean GOOD.** `gate()`'s regression rule is *"was met, now unmet"*, which only
means "worse" under that convention. An inverted detector (met-when-bad, like the shipped `stalled`)
has to be inverted before it is used as a driver judge.

**`step()` stages; `gate()` lands.** A driver that never gates loses nothing — every session is
committed on its own branch and the span tip holds them all — but the base branch never moves
either. A gate on an empty span is free, so calling it unconditionally at the bottom of a `for` is
the right default.

---

## Where to look next

| | |
|---|---|
| `examples/workflow.rs` | the full surface in use — steps, all three judge kinds, `&&` gating, `block()` |
| `examples/selfimprove.rs` | a driver that improves the tool that runs it, and the hazards that come with it |
| `examples/workflow.yaml` | the same workflow in YAML, with an honest list of what it loses |
| `docs/CONFIG.md` | every `agg.yaml` key |
