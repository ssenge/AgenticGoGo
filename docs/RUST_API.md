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
        .limits(Limits { tokens: Some(40_000_000), cost: None,
                         sessions: Some(300), wall_hours: Some(10.0) })
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
| `agg.ask()` `agg.info()` `agg.log()` | the three notification levels |
| `agg.block(q)` | wait for a human on the operator bus (ceilings keep firing while it waits) |

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
can measure and another apply a threshold. The project tree is read-only to every judge: a judge
able to write the tree it grades can make the code pass.

---

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
- **An answered `block()` after the last kept gate will ask again**, and the original answer is not
  recoverable. Gate before you block if that matters.
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
