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
| `.limits()` `.on_regression()` `.ratchet()` `.merge_requires_improvement()` `.instructions()` | the builder chain (self-consuming; build judges *above* it) |
| `.user_settings(bool)` | whether workers load the operator's own agent settings (plugins, MCP servers, hooks, personal instructions). On by default; off, Claude workers start with `--setting-sources project,local`, and an agent that can't leave them out says so |
| `.system_prompt(text)` | text every worker's agent adds to its system prompt, for a rule that must hold for the whole session rather than the first turn (A-34). Claude: `--append-system-prompt`; an agent that can't take it runs without it and says so |
| `.memory(bool)` | the run's default for `Step::memory`: whether sessions keep agg's working memory (forward state, wiki, memory log, summary). On by default; a step's own setting wins |
| `.judge_agent(Agent::Codex)` `.judge_model(m)` `.judge_timeout(secs)` | the ruler — YAML's `judge.agent` / `judge.model` / `judge.timeout` ([CONFIG](CONFIG.md#judge--the-ruler)). `Agent::Custom` is refused when the run boots: the ruler is resolved by name |
| `.context(path, name)` `.network(Network::Allow(..))` `.image()` `.judge_image()` `.pass_env([..])` `.credentials([..])` | the run's confinement: read-only context dirs, the egress allow-list, the container images and what crosses into a container — YAML's `context:` / `isolation_policy:` / `image:` / `judge.image` ([CONFIG](CONFIG.md#network-egress--isolation_policynetwork)) |
| `agg.step(&s)` | run one session. Commits on a session branch — **stages**, never merges |
| `agg.judge(&j)` | this step's verdict, running it if not yet asked. **Memoized per step** |
| `agg.group(&g)` | every member of a `Group`'s verdict, through `judge()`'s memoized cache — count over it: `.at_least(2)`, `.all()`, `.any()` |
| `agg.gate()` | close the span: merge everything staged since the last gate, or discard it per policy |
| `agg.gate_state()` | the gate's own history: ratchet references, `rollbacks`, `consecutive_rollbacks`, `no_improvements`, `consecutive_no_improvements`, `no_changes`, `consecutive_no_changes`, `oscillating(k)` |
| `agg.check_limits()` | enforce ceilings, drain the operator bus, honour Ctrl-C |
| `agg.pos(label, max)` | an RAII breadcrumb — `cycle 3/20 › attempt 2/3` — published to `state.json` |
| `agg.ask()` `agg.info()` `agg.log()` | the three notification levels — non-blocking, and still the default |
| `agg.hil_bool(q)` `hil_choose(q,&[..])` `hil_input(q)` | **ask a human and get the value back.** BLOCKS until answered |
| `agg.open_asks()` | the asks a human still owes an answer to — including ones the WORKER opened |
| `agg.block(q)` | the old doorbell: waits for `agg send resume`, returns nothing. Prefer `hil_bool` |
| `agg.finish(outcome)` `agg.finish_with(outcome, reason)` | end the run explicitly (else dropping `Agg` records `GoalsMet`). `finish_with(RunOutcome::Halt, "rollbacks >= 2")` names the stop condition the driver saw fire, worded as `agg run` words a fired `abort_if` term: report `stop_reason` `ABORT: rollbacks >= 2`, run-ledger end reason `abort:rollbacks >= 2` |

A step's task can live in a file: `Step::new("implement").prompt_file("steps/implement.md")` (relative
to `agg/`, or absolute) is read at every session start and hashed into `agg/private/prompts.jsonl`,
exactly like YAML's `prompt_file:`. `.prompt()` and `.prompt_file()` replace each other — a derived
step overrides its template's choice — and `agg.step()` refuses a step carrying both, or naming a
file it cannot read.

The role works the same way: `.role_prompt(text)` / `.role_prompt_file("roles/tester.md")` are YAML's
per-step `role_prompt:` / `role_prompt_file:`, replace each other, and — set either — replace the run's
defaults for that step; a step that sets neither runs with the defaults.

A step that runs once and hands its result on in its own files can drop agg's working memory:
`Step::new("check").memory(false)`. Its brief then names no `STATE.md`, wiki or memory log, and no memory
fold or summary runs after it. Memory stays on for steps that loop, where one session advises the next.

A step can start from an earlier step's agent session instead of a fresh context:
`agg.step(&review.create("fix").resume(&done.agent_session.unwrap()))` continues it, `.fork(&s)` branches
off it and leaves it as it was, so several steps can start from one session and a rerun starts from the
same context. `StepOutcome::agent_session` is the handle (the agent and its session id). Only the agent that
ran the session can continue it; on another agent, a backend without resume (or fork: only Claude has it),
or a session the agent no longer has, the step says so and starts fresh. The context is a cache, never the
record: a step must still pass started fresh. Not to be confused with resuming a crashed run, below.

### Judges

Three kinds, one type:

```rust
Judge::script("tests_pass", "agg/judges/tests.sh")     // any executable; verdict JSON on stdout
Judge::rubric("spec_sound", "agg/judges/spec.md")      // the .md IS the prompt; graded by the ruler
Judge::rubric("review", "agg/judges/review.md")       // …asking its OWN agent, model and effort,
    .agent(Agent::Codex).model("gpt-5").effort(Effort::Low) // not the run's ruler (a reviewer ≠ the worker)
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

A verdict's `.with_evidence([...])` paths are KEPT: when the driver asks a judge, agg copies each
named file (relative paths resolve in `c.scratch()` first, then the project) to
`agg/private/evidence/<run_id>/<session>/<judge>/` — under the session it was JUDGED in, since a later
`gate()` sees another scratch — and names the copies on the gate's ledger row and in the report.
`Agg::evidence_max_mb(n)` bounds one run's copies (default 100, the twin of `judge.evidence_max_mb`).

⚠ **What confines a driver's judges is `.isolation()` on its STEPS.** There is no separate knob: the
run's tier is the strongest tier any step has declared, and judges take that — never the tier of
whichever step happens to be current, because a judge's confinement follows from its role, not from
its caller.

```rust
let build = Step::new("implement").isolation(Isolation::Sandbox);   // ← this confines the JUDGES too
```

A context dir or a network allow-list needs EVERY step confined: `agg.step()` refuses an
`Isolation::None` step when the run has either, and refuses a step whose agent's endpoint is not on
the allow-list (a driver's steps never reach the YAML startup check, so the rule runs per step).
`Step::image("…")` picks a container step's image; under `Isolation::Container` the judges run in
`.judge_image()` (else `.image()`, else the base image), with the project read-only.

A driver that declares no tier anywhere runs its judges unconfined, exactly as it runs its workers
unconfined. That is the default and it is the honest one — but if you sandbox the worker and not the
judges, the judge script is the way out, because a confined worker can rewrite `agg/judges/*.sh` in
its own writable cwd.

---

### The ratchet and the gate's history

`gate()`'s boolean rule protects `met`. A ratchet protects a NUMBER: a ratcheted judge's `value` may not
get worse than at the last kept gate by more than its tolerance, or the gate counts a regression and,
under `OnRegression::Rollback`, discards the span. Same engine, same body as the YAML
[`sequence.ratchet`](CONFIG.md#the-ratchet--numeric-no-regression):

```rust
let agg = Agg::open(".")?
    .ratchet("cc_p95", Ratchet::down(0.5))                        // lower is better, ±0.5
    .ratchet("mutation", Ratchet::up(1.0).baseline(71.0));        // higher is better, seeded

agg.judge(&cc_p95);              // asked BEFORE the first step() = the driver's baseline pass
for _ in 0..20 {
    agg.step(&implement)?;
    agg.judge(&cc_p95);          // the ratchet applies to the judges ASKED since the last gate
    agg.judge(&mutation);
    agg.gate()?;                 // a regression beyond tolerance ⇒ GateOutcome::RolledBack
    let g = agg.gate_state();
    if g.consecutive_rollbacks >= 3 || g.oscillating(4) { break; }   // YAML's abort_if terms, in Rust
}
```

- **The reference** before the first keep is the seed, else the first value asked before the first
  `step()`; without either, the first kept gate sets it. A kept gate moves it; a rollback never does.
- **A ratcheted judge that errored or emitted no number never passes** — it rolls the span back.
- **`gate_state()`** is what `ratchet_regressed`, `<judge>.ref`, `rollbacks`, `consecutive_rollbacks`,
  `no_improvements`, `no_changes` (and their `consecutive_` forms) and `oscillating` read on the YAML path.
  A span with no commits returns `GateOutcome::Nothing` from `gate()`, but it is still a gate: its rows
  are stamped `no_changes` and it counts for `no_changes` / `consecutive_no_changes` — bound an idle
  worker with `agg.gate_state().consecutive_no_changes >= 3`. The signature `oscillating` compares is every judge asked since the
  last gate that was unmet, plus every ratchet regression.
- **On resume** the state is restored from the last replayed `gate` record — i.e. as of the last kept
  gate — not from `agg/private/gate.json`, which may be ahead of it by gates that will re-execute.

### Metric and score judges (swe-metrics)

The YAML `judges.<name>.metric` / `.score` declarations
([CONFIG](CONFIG.md#judges-built-on-swe-metrics--metric-and-score)) are two more judge constructors —
the same engine, so judges asked in one span share ONE `swe-metrics measure` of the tree:

```rust
use agg::prelude::*;
let cc = Judge::metric("cc_p95", MetricJudge::new("complexity.cyclomatic").stat(Stat::P95).tool("lizard").max(10.0));
let vulns = Judge::metric("vulns", MetricJudge::new("dependencies.vulnerabilities").label("severity", &["critical", "high"]).max(0.0));
let quality = Judge::score("quality", ScoreJudge::new("swe-default", "2").min(80.0));
let agg = Agg::open(".")?.ratchet("cc_p95", Ratchet::down(0.5));   // the driver states the direction
```

`MetricJudge` also takes `.combine(Combine::All)`, `.scope(Scope::Changed)` and `.min(..)`; a relative
`ScoreJudge` definition file is resolved against the project dir. A missing value fails closed exactly
as on the YAML path. When the run's tier is `container` (a step with `Isolation::Container`),
swe-metrics runs in `.judge_image()` like every judge. Two differences: a driver has no startup, so a
missing or incompatible `swe-metrics` shows as an ERRORED verdict, not a refusal; and a driver run has no incumbent until its
first kept gate — a score asked before that is computed without `--baseline`.

### Groups and the improvement rule

A [`Group`](CONFIG.md#judge-groups--k-of-n) names judges that measure one thing; `agg.group(&g)` asks
every member (each through the same memoized cache as `agg.judge()`) and returns a `GroupVerdict` to
count over. The threshold lives where you branch, never on the group:

```rust
let quality = Group::new("quality", [sonar.clone(), codescene.clone(), lizard.clone()]);
let agg = Agg::open(".")?.merge_requires_improvement(true);
for _ in 0..20 {
    agg.step(&implement)?;
    if !agg.group(&quality).at_least(2) { continue; }    // two of three
    let outcome = agg.gate()?;                            // Kept · RolledBack · NoImprovement · …
    if outcome == GateOutcome::NoImprovement && agg.gate_state().consecutive_no_improvements >= 4 {
        break;                                            // four sessions in a row that moved nothing
    }
}
```

- **At `gate()`, a judge asked ONLY through `group()` is judged with its group, net**: one member going
  met→unmet while another goes unmet→met is not a regression; a net loss is. A judge the driver also
  asked by name (`agg.judge`) is held to the per-judge rule. A driver that never asks a group sees no
  change.
- **`merge_requires_improvement(true)`** — `gate()` keeps a span only if a judge asked since the last
  gate went unmet→met (a group: gained ground net) or a ratcheted judge moved the good way beyond its
  tolerance; otherwise it returns `GateOutcome::NoImprovement` and discards the span. That is **not** a
  rollback: `rollbacks` does not move, `no_improvements` / `consecutive_no_improvements` do. A regression
  under `OnRegression::Rollback` wins over it (`RolledBack`).

## Asking a human

Three calls, one behaviour: **block until a human answers.** No timeout, no default, no ending the
run — an idle process spends no tokens, and waiting is cheaper than the machinery that avoids waiting.

```rust
let i  = agg.hil_choose("Which store?", &["postgres", "sqlite"])?;   // -> usize  (closed set)
let v  = agg.hil_input("Which instance is prod?")?;                  // -> String (open set)
let ok = agg.hil_bool("Deploy to prod?")?;                           // -> bool
```

A human answers with `agg answer <id> …` (`agg status` lists open asks and their ids). For a
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

A driver run writes the same `agg/private/report.json` as `agg run` (see
[CONFIG.md](CONFIG.md#the-run-report-aggprivatereportjson)), when the `Agg` drops. `Drop` cannot see
whether `main` returned `Err`, so a driver's own error reads as `done` there — `agg::driver::run`
corrects it to `outcome: error`, which is one more reason to use it. A driver that panics leaves no
report; a consumer reads a missing report as `error`.

## Workflow building blocks (`agg::workflow`)

A process of named steps — a script, an agent's work judged and repaired, a condition, a human's approval —
needs the same few blocks in every driver. `agg::workflow` has them, built on the calls above (no second
execution model). A step's **key** is the first word of its name (`"2.5"` of `"2.5 Resolve bindings"`).

```rust
use agg::prelude::*;
use agg::workflow::{self, Flow, Run, Task, Roles};

fn main() -> std::process::ExitCode {
    workflow::run(drive)                       // starts the run again where triage sent it (--from, --note)
}

fn drive() -> Result<(), Fatal> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let roles = Roles::load("roles.yaml".as_ref(), &["worker", "critic"])?;
    let flow = Flow::new(Agg::open(".")?, Run::from_args(&args)?)
        .history("reports/steps.jsonl")        // default agg/private/steps.jsonl; a tracked path is committed
        .own(["task.md"])                      // what --reset never takes back
        .on_failure(roles.step("worker", "triage", "/abs/triage.md"), 3);
    flow.script_step("1 Fetch", &Judge::script("fetch", "scripts/fetch.sh"))?;
    let design = Task::new(roles.step("worker", "2 Design", "/abs/design.md"))
        .check(Judge::script("design-ok", "judges/design.py"))
        .critic(roles.judge("critic", "design-review", "/abs/design-review.md"))
        .repairs(2);
    if let workflow::Outcome::Accepted(point) = flow.task(&design)? {
        // the critic's point after its repair round: the work is kept; put it before the next approval
    }
    flow.approval("3 Sign-off", "design.md", "Approve the design?", "decisions.md", &[])?;
    Ok(())
}
```

| block | what it does |
|---|---|
| `Flow::new(agg, Run)` | the run: `Run::from_args` reads `--from <key> [--note t] [--reset] [--until <key>] [--reentries n]`; a step before `from` doesn't run, the `from` step gets the note leading its brief, every later step refreshes; `--until` ends the run after that step |
| `.history(path)` `.own(paths)` `.hooks(f)` `.on_start(f)` `.on_failure(step, max)` | the step history; files `--reset` keeps; checks before and after every step (recorded as `hook`); what the driver does as a step starts; triage |
| `flow.script_step(name, &judge)` | a script does the step; met: its changes are committed, recorded `tool`; unmet: the step fails |
| `flow.applies(name, &when)` | the step applies when `when` is met; otherwise recorded `skipped` with the reason, no session; a condition that errs fails the step |
| `flow.task(&Task)` | an agent step: `Task::new(step)` `.check(j)` (several, in order) `.critic(j)` `.critic_context(t)` `.repairs(n)` `.first(script)` `.inputs(paths)` `.context(key)` `.section(text)`; returns `Outcome::{Done, Unchanged, Accepted(point), NotReached}` |
| `flow.approval(name, doc, question, decisions, inputs)` | a human approves `doc`, writing their decisions first; after a re-entry, unchanged inputs (else the document) stand without asking |
| `workflow::run(drive)` | `driver::run`, then starts the run again at the step triage chose, with its note |
| `Roles::load(file, names)` | named roles (`agent`, `model`, `effort`) from a YAML file; an agent that isn't installed is an error at load |

An agent step, in order: a refresh whose `inputs` didn't change is `unchanged` without a session; the `first`
script does what it can (nothing left and the judges met: done without a session); the worker's brief gets
what is new first (the note, the changes since its result, the review to address), then its task, the
driver's sections, the critic's rubric, the checks to run before finishing and a file for "I can't fix this";
the session continues the step's own earlier one, else forks its `context` step's (A-33); then the checks in
order, then the critic with the worker's task below its rubric. A repair resumes the session with the review
leading the brief and a file for disputes; the critic re-reviews only its earlier points and gets one repair
round, after which its open point is accepted and returned. No further round when the worker can't fix it or
a check fails exactly as before. A failed step goes to triage: its agent writes `reenter`, `cause` and `note`
to the file its brief names; an approval is never a re-entry point; a yes ends the run, and
`workflow::run` starts it again.

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
