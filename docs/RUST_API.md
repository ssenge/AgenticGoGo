# The Rust driver API

**This is heavier and Rust-only — use `rlt.yaml` unless you need flow it cannot express.**

The YAML path is a *list of steps that laps forever* until `done_if` fires. That covers most of what
people build. The Rust API exists for the runs where the shape of the work is a program: a branch
that depends on a verifier's number, a retry ladder whose bound changes with what it learned, a loop
that asks an expensive verifier only when three cheap ones already passed.

Everything below is the same engine. `rlt.step()` dispatches through the identical primitive the
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

## A process of standard steps: a process file

When the flow is a **process** — phases of steps, each a script, an agent's work verified by checks and a critic, a human's
approval, a loop over a list or an exam — it needs no Rust either: write a **process file** and run `rlt process <file>
<project>` ([PROCESS.md](PROCESS.md)). It composes the blocks of [`relentless::workflow`](#workflow-building-blocks-aggworkflow); the YAML has no logic of its own.

## When it is not

Four things YAML deliberately cannot do, each of which is a `for`/`if` in Rust:

| you want | YAML | Rust |
|---|---|---|
| skip a step based on a verdict | ✗ — a lap runs every entry (`if:` was cut) | `if rlt.verifier(&x).met() { continue }` |
| **not run** an expensive verifier unless cheap ones pass | ✗ — every run-set verifier runs after every verified step | `rlt.verifier(&cheap).met() && rlt.verifier(&slow).met()` |
| a bound that changes with what the run learned | ✗ — `max:` is a literal | any expression |
| state that changes *within* a cycle | ✗ — verdict rows land per gate | a local variable |

The second row is the one that costs money. A 40-minute load test in a YAML run-set executes after
**every** verified step; in a driver, `&&` short-circuits and it runs only where it matters.

---

## The surface

Eleven calls. `Relentless::open(dir)` then a self-consuming builder chain, then ordinary Rust.

```rust
use relentless::driver::{Relentless, Verifier, Limits, OnRegression, Step, Verdict};

fn main() -> Result<(), relentless::driver::Fatal> {
    let rlt = Relentless::open(".")?
        // SECONDS. `work_time` excludes time blocked on a human — see "Asking a human" below.
        .limits(Limits { tokens: Some(40_000_000), cost: None, sessions: Some(300),
                         wall_time: Some(10.0 * 3600.0), work_time: None })
        .on_regression(OnRegression::Rollback);

    let implement = Step::new("implement").prompt("Make the failing tests pass.");
    let tests     = Verifier::script("tests_pass", "rlt/verifiers/tests.sh");
    let slow      = Verifier::script("load_ok", "rlt/verifiers/loadtest.sh").timeout(45 * 60);

    let cycle = rlt.pos("cycle", 20);          // the breadcrumb readers render
    for c in 1..=20 {
        cycle.update(c);
        rlt.check_limits()?;                   // ceilings are OPT-IN — this is where they fire

        rlt.step(&implement)?;                 // stages work on a session branch; nothing merged
        if !(rlt.verifier(&tests).met() && rlt.verifier(&slow).met()) {
            continue;                          // `&&` IS the cost gate: `slow` never runs when red
        }
        rlt.gate()?;                           // close the span: merge it, or apply on_regression
    }
    Ok(())
}
```

| call | what it does |
|---|---|
| `Relentless::open(dir)` | opens the project; normalises git; **boots lazily** — nothing is published until real work happens |
| `.limits()` `.on_regression()` `.ratchet()` `.merge_requires_improvement()` `.instructions()` | the builder chain (self-consuming; build verifiers *above* it) |
| `.user_settings(bool)` | whether workers load the operator's own agent settings (plugins, MCP servers, hooks, personal instructions). On by default; off, Claude workers start with `--setting-sources project,local`, and an agent that can't leave them out says so |
| `.user_env(false \| true \| [keys])` | which keys of the operator's settings `env` block (`$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`) every Claude worker and one-shot call gets as environment variables without the rest of those settings: none (default), every key, or the named ones. A variable rlt's own environment has wins; `RLT_*` keys are never taken; string values only; only names are logged. Process-wide like `RLT_MODELS`. Other agents: no-op |
| `.reachable(&step)` | one tiny call with the step's agent and model, its settings and `user_env` as the run has them, before the run starts: `Ok` with what answered, or an error naming the agent's cause (and, for an authentication error where the operator's settings hold `ANTHROPIC_*` keys the run does not pass, the setting that passes them). `rlt process` calls it before its first step (`rlt: { preflight: false }` skips it) |
| `.system_prompt(text)` | text every worker's agent adds to its system prompt, for a rule that must hold for the whole session rather than the first turn (A-34). Claude: `--append-system-prompt`; an agent that can't take it runs without it and says so |
| `.vars([(name, value), ..])` | the workflow's variables: `{{ name }}` in every step's prompt **file** (the role prompt's too) and every rubric verifier's rubric (its `inputs:` frontmatter too) is filled in where rlt reads it; a name the map lacks fails that step or verifier. Inline prompts a driver builds aren't templates (they may carry a project's own `{{ … }}`; code has `format!`), and other `{{` (no plain name inside) stays as it is. `relentless::prelude::render(text, &vars)` renders a file of the driver's own (its config) the same way |
| `.memory(bool)` | the run's default for `Step::memory`: whether sessions keep rlt's working memory (forward state, wiki, memory log, summary). On by default; a step's own setting wins |
| `.verifier_agent(Agent::Codex)` `.verifier_model(m)` `.verifier_timeout(secs)` | the ruler — YAML's `verifier.agent` / `verifier.model` / `verifier.timeout` ([CONFIG](CONFIG.md#verifier--the-ruler)). `Agent::Custom` is refused when the run boots: the ruler is resolved by name |
| `.context(path, name)` `.network(Network::Allow(..))` `.image()` `.verifier_image()` `.pass_env([..])` `.credentials([..])` | the run's confinement: read-only context dirs, the egress allow-list, the container images and what crosses into a container — YAML's `context:` / `isolation_policy:` / `image:` / `verifier.image` ([CONFIG](CONFIG.md#network-egress--isolation_policynetwork)) |
| `rlt.step(&s)` | run one session. Commits on a session branch — **stages**, never merges |
| `rlt.verifier(&j)` | this step's verdict, running it if not yet asked. **Memoized per step** |
| `rlt.group(&g)` | every member of a `Group`'s verdict, through `verifier()`'s memoized cache — count over it: `.at_least(2)`, `.all()`, `.any()` |
| `rlt.gate()` | close the span: merge everything staged since the last gate, or discard it per policy |
| `rlt.gate_state()` | the gate's own history: ratchet references, `rollbacks`, `consecutive_rollbacks`, `no_improvements`, `consecutive_no_improvements`, `no_changes`, `consecutive_no_changes`, `oscillating(k)` |
| `rlt.check_limits()` | enforce ceilings, drain the operator bus, honour Ctrl-C |
| `rlt.pos(label, max)` | an RAII breadcrumb — `cycle 3/20 › attempt 2/3` — published to `state.json` |
| `rlt.ask()` `rlt.info()` `rlt.log()` | the three notification levels — non-blocking, and still the default |
| `rlt.hil_bool(q)` `hil_choose(q,&[..])` `hil_input(q)` | **ask a human and get the value back.** BLOCKS until answered |
| `rlt.open_asks()` | the asks a human still owes an answer to — including ones the WORKER opened |
| `rlt.block(q)` | the old doorbell: waits for `rlt send resume`, returns nothing. Prefer `hil_bool` |
| `rlt.finish(outcome)` `rlt.finish_with(outcome, reason)` | end the run explicitly (else dropping `Relentless` records `GoalsMet`). `finish_with(RunOutcome::Halt, "rollbacks >= 2")` names the stop condition the driver saw fire, worded as `rlt run` words a fired `abort_if` term: report `stop_reason` `ABORT: rollbacks >= 2`, run-ledger end reason `abort:rollbacks >= 2` |

A step's task can live in a file: `Step::new("implement").prompt_file("steps/implement.md")` (relative
to `rlt/`, or absolute) is read at every session start and hashed into `rlt/private/prompts.jsonl`,
exactly like YAML's `prompt_file:`. `.prompt()` and `.prompt_file()` replace each other — a derived
step overrides its template's choice — and `rlt.step()` refuses a step carrying both, or naming a
file it cannot read.

The role works the same way: `.role_prompt(text)` / `.role_prompt_file("roles/tester.md")` are YAML's
per-step `role_prompt:` / `role_prompt_file:`, replace each other, and — set either — replace the run's
defaults for that step; a step that sets neither runs with the defaults.

A step that runs once and hands its result on in its own files can drop rlt's working memory:
`Step::new("check").memory(false)`. Its brief then names no `STATE.md`, wiki or memory log, and no memory
fold or summary runs after it. Memory stays on for steps that loop, where one session advises the next.

`.retry(Retry { attempts: 5, ..Retry::default() })`, `.no_retry()` and `.timeout(Duration::from_secs(2700))` set how a session that
failed for no fault of the work (an overload, a dropped connection, a crash, a hang) is tried again; a retry is one
ledger record with `attempts`, not a session ([CONFIG.md](CONFIG.md#retries-and-timeouts)). `rlt.min_free_disk(Floor::Bytes(…))`
sets the free-disk floor the run stops below (default 10 GB, `Floor::Off` for none).

`.tools(["Bash", "Read", "Edit", "Write"])` limits a worker to these tools of its agent, by the agent's own
names; its MCP servers' tools come on top. A session then starts with only those tools' descriptions in its
context. Claude: `--tools`; an agent that can't limit its tools runs with all of them and the step says so.

A step can start from an earlier step's agent session instead of a fresh context:
`rlt.step(&review.create("fix").resume(&done.agent_session.unwrap()))` continues it, `.fork(&s)` branches
off it and leaves it as it was, so several steps can start from one session and a rerun starts from the
same context. `StepOutcome::agent_session` is the handle (the agent and its session id). Only the agent that
ran the session can continue it; on another agent, a backend without resume (or fork: only Claude has it),
or a session the agent no longer has, the step says so and starts fresh. The context is a cache, never the
record: a step must still pass started fresh. Not to be confused with resuming a crashed run, below.
`.news(text)` names what is new for this attempt (also written into its brief): a session that continues its
own gets it in its opening message after the pointer to the brief (cut at 4000 characters), since it takes
the rest of the brief for known; a fresh or forked session gets the pointer alone.

### Verifiers

Three kinds, one type:

```rust
Verifier::script("tests_pass", "rlt/verifiers/tests.sh")     // any executable; verdict JSON on stdout
Verifier::rubric("spec_sound", "rlt/verifiers/spec.md")      // the .md IS the prompt; graded by the ruler
Verifier::rubric("review", "rlt/verifiers/review.md")       // …asking its OWN agent, model and effort,
    .agent(Agent::Codex).model("gpt-5").effort(Effort::Low) // not the run's ruler (a reviewer ≠ the worker)
Verifier::native("p99_ok", |c| {                          // a Rust closure over a VerifierCtx
    let ms = std::fs::read_to_string(c.scratch().join("bench.json"))
        .ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["p99_ms"].as_f64()).unwrap_or(f64::MAX);
    Verdict::binary(ms <= 5.0).with_value(ms).with_rationale(format!("p99 {ms}ms"))
})
```

⛔ `VerifierCtx` offers **no clock, no randomness, no network and no environment** — deliberately. A
verifier that reads the wall clock returns a different verdict on replay, and resume would silently
diverge from the run it claims to reproduce. A check that genuinely needs the clock is a
`Verifier::script`, where the impurity sits in a file a reviewer can see.

`c.scratch()` is the per-session directory verifiers may write — **shared between them**, so one verifier
can measure and another apply a threshold. A verifier able to write the tree it grades can make the code
pass, so under a confining tier the project tree is read-only to it and writes are relocated:
`$RLT_VERIFIER_SCRATCH` for working files, a persistent per-project dir for toolchain caches
(`CARGO_TARGET_DIR`, `PYTHONPYCACHEPREFIX`, `GOCACHE`, …). Full table in
[docs/CONFIG.md](CONFIG.md#-a-verifier-may-not-write-the-project-tree).

A verdict's `.with_evidence([...])` paths are KEPT: when the driver asks a verifier, rlt copies each
named file (relative paths resolve in `c.scratch()` first, then the project) to
`rlt/private/evidence/<run_id>/<session>/<verifier>/` — under the session it was VERIFIED in, since a later
`gate()` sees another scratch — and names the copies on the gate's ledger row and in the report.
`Relentless::evidence_max_mb(n)` bounds one run's copies (default 100, the twin of `verifier.evidence_max_mb`).

⚠ **What confines a driver's verifiers is `.isolation()` on its STEPS.** There is no separate knob: the
run's tier is the strongest tier any step has declared, and verifiers take that — never the tier of
whichever step happens to be current, because a verifier's confinement follows from its role, not from
its caller.

```rust
let build = Step::new("implement").isolation(Isolation::Sandbox);   // ← this confines the VERIFIERS too
```

A context dir or a network allow-list needs EVERY step confined: `rlt.step()` refuses an
`Isolation::None` step when the run has either, and refuses a step whose agent's endpoint is not on
the allow-list (a driver's steps never reach the YAML startup check, so the rule runs per step).
`Step::image("…")` picks a container step's image; under `Isolation::Container` the verifiers run in
`.verifier_image()` (else `.image()`, else the base image), with the project read-only.

A driver that declares no tier anywhere runs its verifiers unconfined, exactly as it runs its workers
unconfined. That is the default and it is the honest one — but if you sandbox the worker and not the
verifiers, the verifier script is the way out, because a confined worker can rewrite `rlt/verifiers/*.sh` in
its own writable cwd.

---

### The ratchet and the gate's history

`gate()`'s boolean rule protects `met`. A ratchet protects a NUMBER: a ratcheted verifier's `value` may not
get worse than at the last kept gate by more than its tolerance, or the gate counts a regression and,
under `OnRegression::Rollback`, discards the span. Same engine, same body as the YAML
[`sequence.ratchet`](CONFIG.md#the-ratchet--numeric-no-regression):

```rust
let rlt = Relentless::open(".")?
    .ratchet("cc_p95", Ratchet::down(0.5))                        // lower is better, ±0.5
    .ratchet("mutation", Ratchet::up(1.0).baseline(71.0));        // higher is better, seeded

rlt.verifier(&cc_p95);              // asked BEFORE the first step() = the driver's baseline pass
for _ in 0..20 {
    rlt.step(&implement)?;
    rlt.verifier(&cc_p95);          // the ratchet applies to the verifiers ASKED since the last gate
    rlt.verifier(&mutation);
    rlt.gate()?;                 // a regression beyond tolerance ⇒ GateOutcome::RolledBack
    let g = rlt.gate_state();
    if g.consecutive_rollbacks >= 3 || g.oscillating(4) { break; }   // YAML's abort_if terms, in Rust
}
```

- **The reference** before the first keep is the seed, else the first value asked before the first
  `step()`; without either, the first kept gate sets it. A kept gate moves it; a rollback never does.
- **A ratcheted verifier that errored or emitted no number never passes** — it rolls the span back.
- **`gate_state()`** is what `ratchet_regressed`, `<verifier>.ref`, `rollbacks`, `consecutive_rollbacks`,
  `no_improvements`, `no_changes` (and their `consecutive_` forms) and `oscillating` read on the YAML path.
  A span with no commits returns `GateOutcome::Nothing` from `gate()`, but it is still a gate: its rows
  are stamped `no_changes` and it counts for `no_changes` / `consecutive_no_changes` — bound an idle
  worker with `rlt.gate_state().consecutive_no_changes >= 3`. The signature `oscillating` compares is every verifier asked since the
  last gate that was unmet, plus every ratchet regression.
- **On resume** the state is restored from the last replayed `gate` record — i.e. as of the last kept
  gate — not from `rlt/private/gate.json`, which may be ahead of it by gates that will re-execute.

### Metric and score verifiers (swe-metrics)

The YAML `verifiers.<name>.metric` / `.score` declarations
([CONFIG](CONFIG.md#verifiers-built-on-swe-metrics--metric-and-score)) are two more verifier constructors —
the same engine, so verifiers asked in one span share ONE `swe-metrics measure` of the tree:

```rust
use relentless::prelude::*;
let cc = Verifier::metric("cc_p95", MetricVerifier::new("complexity.cyclomatic").stat(Stat::P95).tool("lizard").max(10.0));
let vulns = Verifier::metric("vulns", MetricVerifier::new("dependencies.vulnerabilities").label("severity", &["critical", "high"]).max(0.0));
let quality = Verifier::score("quality", ScoreVerifier::new("swe-default", "2").min(80.0));
let rlt = Relentless::open(".")?.ratchet("cc_p95", Ratchet::down(0.5));   // the driver states the direction
```

`MetricVerifier` also takes `.combine(Combine::All)`, `.scope(Scope::Changed)` and `.min(..)`; `ScoreVerifier` takes
`.scope(Scope::Path("work".into()))` (never `Scope::Changed`), and both take `Scope::ChangedIn { path, since }`:
the files of `path` changed since the Git ref `since` (a step's own changes; a score of them has no incumbent,
and is vacuously met when nothing changed). `ScoreVerifier::reports(script)` scores the build's reports too (see
`score.reports` in CONFIG.md); a relative
`ScoreVerifier` definition file is resolved against the project dir. A missing value fails closed exactly
as on the YAML path. When the run's tier is `container` (a step with `Isolation::Container`),
swe-metrics runs in `.verifier_image()` like every verifier. Two differences: a driver has no startup, so a
missing or incompatible `swe-metrics` shows as an ERRORED verdict, not a refusal; and a driver run has no incumbent until its
first kept gate — a score asked before that is computed without `--baseline`.

### Groups and the improvement rule

A [`Group`](CONFIG.md#verifier-groups--k-of-n) names verifiers that measure one thing; `rlt.group(&g)` asks
every member (each through the same memoized cache as `rlt.verifier()`) and returns a `GroupVerdict` to
count over. The threshold lives where you branch, never on the group:

```rust
let quality = Group::new("quality", [sonar.clone(), codescene.clone(), lizard.clone()]);
let rlt = Relentless::open(".")?.merge_requires_improvement(true);
for _ in 0..20 {
    rlt.step(&implement)?;
    if !rlt.group(&quality).at_least(2) { continue; }    // two of three
    let outcome = rlt.gate()?;                            // Kept · RolledBack · NoImprovement · …
    if outcome == GateOutcome::NoImprovement && rlt.gate_state().consecutive_no_improvements >= 4 {
        break;                                            // four sessions in a row that moved nothing
    }
}
```

- **At `gate()`, a verifier asked ONLY through `group()` is verified with its group, net**: one member going
  met→unmet while another goes unmet→met is not a regression; a net loss is. A verifier the driver also
  asked by name (`rlt.verifier`) is held to the per-verifier rule. A driver that never asks a group sees no
  change.
- **`merge_requires_improvement(true)`** — `gate()` keeps a span only if a verifier asked since the last
  gate went unmet→met (a group: gained ground net) or a ratcheted verifier moved the good way beyond its
  tolerance; otherwise it returns `GateOutcome::NoImprovement` and discards the span. That is **not** a
  rollback: `rollbacks` does not move, `no_improvements` / `consecutive_no_improvements` do. A regression
  under `OnRegression::Rollback` wins over it (`RolledBack`).

## Asking a human

Three calls, one behaviour: **block until a human answers.** No timeout, no default, no ending the
run — an idle process spends no tokens, and waiting is cheaper than the machinery that avoids waiting.

```rust
let i  = rlt.hil_choose("Which store?", &["postgres", "sqlite"])?;   // -> usize  (closed set)
let v  = rlt.hil_input("Which instance is prod?")?;                  // -> String (open set)
let ok = rlt.hil_bool("Deploy to prod?")?;                           // -> bool
```

A human answers with `rlt answer <id> …` (`rlt status` lists open asks and their ids). For a
`choose`/`bool` ask the value must be on the recorded list, so the CLI refuses anything else and this
call cannot hand you a value you did not offer — that is why `hil_choose` exists next to `hil_input`.

⛔ **`rlt.yaml` has no `hil` key and the worker cannot reach these.** `rlt hil` records an ask and
exits; only a driver author, at a call site they wrote, can make the loop wait. With no bound on the
wait, that asymmetry is the only thing standing between this and the interactive agent rlt replaces.

Four rules, and they are the whole design:

1. **A human's answer unblocks the STEP; a verifier still owns the VERDICT.** Never let a "done" satisfy
   a goal — `while !rlt.verifier(&dns_ok).met() { rlt.hil_bool("created the record?")?; }`.
2. **An answer may NAME a secret, never CONTAIN one.** `hil_input` writes to the ask ledger and the
   next session's brief. Ask for the credential to be *placed*, and confirm with `hil_bool`.
3. **`gate()` before you block for a human who will touch the tree.** `step()` only stages, so a
   human editing an open span collides with staged work. rlt warns; it cannot fix it for you.
4. **Set `work_time`, not just `wall_time`.** Ceilings keep firing while blocked, so an overnight
   question would otherwise consume a ceiling meant to measure the agent's effort. `work_time`
   excludes human wait; `wall_time` is a genuine end-to-end deadline.

`rlt stop` and Ctrl-C interrupt a wait — the bus is drained on every poll — which is why no timeout is
needed to escape a question nobody will answer.

### Exit codes

`fn main() -> Result<(), Fatal>` collapses **every** ending to exit 1: Rust's `Termination` impl
prints `Error: {:?}` and returns `FAILURE`, so `rlt stop`, a blown ceiling and a genuine panic all
look identical to a wrapper. Use `relentless::driver::run`:

```rust
fn main() -> std::process::ExitCode { relentless::driver::run(real_main) }
fn real_main() -> Result<(), Fatal> { /* … */ }
```

It maps the ending to the same codes `rlt run` uses: **0** goals met · **3** `abort_if` · **4**
max-sessions · **5** stopped by an operator · **7** free disk below the floor (resumable) · **1** a real fault. `relentless::workflow::run` adds **6**: a halt
because the task force's re-entries and repairs for the failed step are spent (a restart of that step halts
again; something it works on must change first).

A driver run writes the same `rlt/private/report.json` as `rlt run` (see
[CONFIG.md](CONFIG.md#the-run-report-aggprivatereportjson)), when the `Relentless` drops. `Drop` cannot see
whether `main` returned `Err`, so a driver's own error reads as `done` there — `relentless::driver::run`
corrects it to `outcome: error`, which is one more reason to use it. A driver that panics leaves no
report; a consumer reads a missing report as `error`.

## Workflow building blocks (`relentless::workflow`)

A process of named steps — a script, an agent's work verified and repaired, a condition, a human's approval —
needs the same few blocks in every driver. `relentless::workflow` has them, built on the calls above (no second
execution model). A step's **key** is the first word of its name (`"2.5"` of `"2.5 Resolve bindings"`).

```rust
use relentless::prelude::*;
use relentless::workflow::{self, Flow, Run, Task, Roles};

fn main() -> std::process::ExitCode {
    workflow::run(drive)                       // starts the run again where the task force sent it (--from, --note)
}

fn drive() -> Result<(), Fatal> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let roles = Roles::load("roles.yaml".as_ref(), &["worker", "critic"])?;
    let flow = Flow::new(Relentless::open(".")?, Run::from_args(&args)?)
        .history("reports/steps.jsonl")        // default rlt/private/steps.jsonl; a tracked path is committed
        .own(["task.md"])                      // what --reset never takes back
        .on_failure(roles.step("worker", "task-force", "/abs/task-force.md"), 3);
    flow.script_step("1 Fetch", &Verifier::script("fetch", "scripts/fetch.sh"))?;
    let design = Task::new(roles.step("worker", "2 Design", "/abs/design.md"))
        .check(Verifier::script("design-ok", "verifiers/design.py"))
        .critic(roles.verifier("critic", "design-review", "/abs/design-review.md"))
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
| `Flow::new(rlt, Run)` | the run: `Run::from_args` reads `--from <key> [--note t | --note-file f [--decided]] [--reset] [--until <key>] [--reentries n]`; a step before `from` doesn't run, the `from` step gets the note leading its brief (`--decided`: the note is the humans' decision, and the `from` step refreshes against it like a later step, unchanged when it doesn't concern it), every later step refreshes; `--until` ends the run after that step |
| `.history(path)` `.own(paths)` `.hooks(f)` `.on_start(f)` `.on_failure(step, max)` `.trust(rule)` | the step history; files `--reset` keeps; checks before and after every step (recorded as `hook`); what the driver does as a step starts; the task force (at most `max` re-entries and repairs in place per failed step of a run, kept in `rlt/private/task-force.json` across the driver's processes, a restart `--from` a step included; a run from the start begins the count anew; past it the run exits 6); which of the task force's proposals go ahead without a human's yes (`rule(failed step, re-enter at)`) |
| `.order(keys)` `.constraints(f)` | every step key of the driver in order, built or not, so `--until` ends the run before the first step after it even when the steps up to it never run; what a failed step must keep to (`f(flow, step) -> Option<String>`: its scope, its rubric), in the task force's brief |
| `flow.script_step(name, &verifier)` | a script does the step; met: its changes are committed, recorded `tool`; unmet: the step fails, and after the task force's repair in place the script runs again |
| `flow.recover(name, why, &again)` | the driver's own failure that `again` can verifier again (`Ok(None)`: met now): the task force may repair it in place, and when `again` is met the call returns and the step goes on; `flow.fail(name, why)` for one it can't verifier again |
| `flow.applies(name, &when)` | the step applies when `when` is met; otherwise recorded `skipped` with the reason, no session; a condition that errs fails the step |
| `flow.task(&Task)` | an agent step: `Task::new(step)` `.check(j)` (several, in order) `.critic(j)` `.critic_context(t)` `.repairs(n)` `.first(script)` `.inputs(paths)` `.section(text)` `.voices(roles)`; returns `Outcome::{Done, Unchanged, Accepted(point), NotReached}` |
| `flow.approval(name, doc, question, decisions, inputs)` | a human approves `doc`, writing their decisions first; after a re-entry, unchanged inputs (else the document) stand without asking |
| `flow.reenter(key, note)` | the driver itself sends the run back with a decision: it ends this run and starts it again at a step the run has seen, with `note` and `--decided`, as after a proposal a human said yes to (counts as a re-entry, recorded `reentry`); e.g. a decision taken at an approval that an earlier step's result doesn't carry yet |
| `workflow::run(drive)` | `driver::run`, then starts the run again at the step the task force or `reenter` chose, with its note |
| `Roles::load(file, names)` | named roles (`agent`, `model`, `effort`) from a YAML file; an agent that isn't installed is an error at load |

An agent step, in order: a refresh whose `inputs` didn't change is `unchanged` without a session; the `first`
script does what it can (nothing left and the verifiers met: done without a session); the worker's brief gets
what is new first (the note, the changes since its result, the review to address), then its task, the
driver's sections, the critic's rubric, the checks to run before finishing and a file for "I can't fix this";
every session starts fresh, its repairs' too: what a step knows is in its brief and the project's files; then
the checks in order, then the critic with the worker's task below its rubric. Only a step with `inputs` is a
refresh after a re-entry (one without them runs its task whole), and its changes are those to its inputs. A
repair is a fresh session with the review leading the brief and a file for disputes; the critic re-reviews only its earlier points and gets one repair
round, after which its open point is accepted and returned. No further round when the worker can't fix it, a
check fails exactly as before, or a check's verdict is `blocked_by` another step's result. A failed step goes
to the task force: its agent writes `outcome`, `reenter`, `cause` and `note` to the file its brief names. `fixed`: it
repaired the cause in place, and the step's verifiers (a script step: its script) verifier it again; met, the step
is done and the run goes on, recorded `task-force` with the files it changed; still unmet, the run ends.
`reenter`: an approval is never a re-entry point; a yes ends the run, and `workflow::run` starts it again.
`escalate` (or no step): the run ends for a human. Repairs in place and re-entries share the task force's limit.

A task with `voices` (roles: agent, model, effort) is discussed before the worker's session (A-36): each voice
writes a position on the task on its own, then in turn each reads the others' latest, agrees or disagrees with
evidence, keeps, changes or drops its own points (for evidence, not to meet the others halfway) and ends with
`DISAGREE: <n>`, the others' points it still disputes. The discussion ends when every voice says 0, or after
three rounds. The voices read the task with its sections but not how it is reviewed, and change nothing in the
project (their positions are in the flow's scratch folder); the worker then gets their last positions, carries
out what they agree on and names each point it decided where they don't. Recorded `discussion` (met when they
agreed); a refresh doesn't discuss again; a voice that writes no position fails the step. Worth its sessions
where one agent's blind spots cost most, with voices of comparable strength: a weaker voice tends to defer.

The files a brief names (`flow.scratch(file)`, `flow.wrapper(file, command)`) live in the flow's own folder
outside the project, `rlt-flow-<pid>-<n>` in the temp dir. It is removed when the flow is dropped, however the
run ended, and a flow that starts removes the folders of runs whose process is gone: a file a dead run left
there would read as current to a later run's agent.

## Resuming a crashed run

An overnight driver that dies at hour six should not start again at hour zero.

```rust
let rlt = Relentless::open_with(".", Opts { resume: true })?;   // instead of Relentless::open(".")
```

Every completed call appended a line to `rlt/private/calls.jsonl`, and on resume the same calls are
answered from that file — no worker, no ruler, no git, no tokens. **Your loop is not serialized and
does not need to be:** it runs from the top and its own `if`/`for` walk it back to where it stopped,
because identical inputs produce identical branches.

⚠ **Fast-forward reaches back only as far as the last `gate()` that returned `Kept`.** Everything
after it is discarded and re-executes. That is not conservatism: `step()` always *stages*, so until
a gate lands it the work is on a per-run branch the ledger cannot describe — replaying such a step
as "done" would leave the next session building on an orphaned ref. rlt prints exactly what it
drops.

Three consequences worth designing around:

- **Side effects outside `rlt.*` re-execute.** A `println!`, a file write or an HTTP POST in your
  loop body happens again during replay. Put once-only effects behind an `rlt.*` call (`log`, `info`,
  `ask`, `block` are all recorded) or make them idempotent.
- **An answered `hil_*`/`block()` after the last kept gate will ask again**, and the original answer
  is not recoverable — the one resume cost paid by a person rather than by tokens. Gate before you ask
  if that matters; rlt prints exactly which human calls it dropped.
- **Don't branch on the clock, `rand`, or an env var.** Anything derived from a `Verdict` or a
  `StepOutcome` is safe by construction; anything else can send the replay down a different path,
  and rlt will refuse rather than guess — a call that does not match the record aborts the resume
  naming both sides.

## Forks: parallel work

Work that doesn't depend on each other can run at once. The parallelism is plain Rust (`rayon`, `std::thread::scope`, a thread
pool, tokio's blocking pool: `Relentless` is `Send + Sync`); what the engine adds is the one thing Rust can't know: two agents in
one folder overwrite each other's files and git branch. So each parallel piece of work gets **a copy of the project of its own**,
and its result comes back through git:

- `rlt.fork()?` is a new `Relentless` in a worktree of its own, on a branch of its own cut from what `rlt` has kept. It has `rlt`'s
  settings and is a full instance: it steps, gates, verifies, and forks again.
- `rlt.merge(fork)?` merges the fork's branch into `rlt`'s (`git merge --no-ff`, subject `rlt: merge fork <name>`) and returns the
  merge commit. A conflict is aborted and comes back as `Fatal::Conflict { branch, files }`; `rlt` is as it was and the branch is kept.
- A fork that is **dropped** instead of merged is **discarded**: its worktree goes, its branch stays.

```rust
let mut rlt = Relentless::open(".")?;

// different steps at once
let (guide, slides) = (rlt.fork()?, rlt.fork()?);
let (a, b) = rayon::join(|| guide.step(&write_guide), || slides.step(&write_slides));
a?; b?;
rlt.merge(guide)?;
rlt.merge(slides)?;

// the best of three tries
let tries = [rlt.fork()?, rlt.fork()?, rlt.fork()?];
tries.par_iter().for_each(|t| { let _ = t.step(&fix_the_bug).and_then(|_| t.gate()); });
let best = tries.into_iter().find(|t| t.verifier(&tests_pass).met()).context("no try passed")?;
rlt.merge(best)?;   // the other tries are dropped, so they are discarded

// many items, at most 4 at once: the pool's size is the limit
let forks: Vec<_> = units.iter().map(|_| rlt.fork()).collect::<Result<_, _>>()?;
rayon::ThreadPoolBuilder::new().num_threads(4).build()?.install(|| {
    forks.par_iter().zip(&units).for_each(|(f, u)| { let _ = f.step(&port(u)).and_then(|_| f.gate()); })
});
for f in forks { rlt.merge(f)?; }
```

`examples/forks.rs` runs these with plain functions in place of agent steps. The rules:

1. **`fork` and `merge` take `&mut self`**, so a driver forks and merges on the thread that owns the instance, in the order its
   code has. A fork is named by the number its call has in the run's ledger (`4`; a fork of the fork `4` is `4-2`), which is how a
   resumed run finds it again. The worktree is `rlt/private/lanes/fork-<name>/`, the branch `rlt/<project>/lane-fork-<name>`, its
   sessions `rlt/<project>-fork-<name>/session-N`.
2. **Gate first.** `fork` refuses while the instance has ungated work (a fork starts from what the run has *kept*), and `merge` refuses
   while either side has some. The project must be a git repository, and the tree must be clean when the first fork starts.
3. **The worktree is made at the fork's first call**, not at `fork()`: forking fifty items and running four at a time makes four at a
   time. `Relentless::fork_seed(["node_modules/"])` names ignored or untracked paths (dependencies, build caches) that every fork's
   worktree is given, as it starts from what git holds.
4. **One thread at a time per instance.** The same thread may nest calls; a call from a second thread while it is busy panics
   ("give each thread its own fork"), as two sessions in one worktree would corrupt each other. A fork carries its parent's settings
   and variables to whichever thread runs it.
5. **One run, seen from outside.** `rlt stop`, `pause`, `resume`, `wake` and `inject` sent to the run reach every fork (each applies
   them at its next boundary, as a run does); a fork's `hil_*` and `block` questions are in the run's list of asks, marked
   `[fork <name>]`, so `rlt answer` and `rlt status` see them; `rlt/private/lanes.json` (shown by `rlt status`) lists the forks. What
   the forks spend counts toward the run's `limits` and a rate limit one meets holds back the others.
6. **Resume.** A fork that was merged or dropped replays from its saved ledger (`rlt/private/forks/<name>/`) with no agent call; one
   that was still running continues after its last kept gate. The forks' spend is kept in `rlt/private/forks/spend.json`, so a
   resumed run's ceilings count what they spent before; work run again after a resume counts twice, which errs on the safe side.

Not there yet: forks on another machine from Rust (use a process file's `parallel.target`), and ending one fork from code.

## Two rules that are easy to get wrong

**A verifier's `met` must mean GOOD.** `gate()`'s regression rule is *"was met, now unmet"*, which only
means "worse" under that convention. An inverted detector (met-when-bad, like the shipped `stalled`)
has to be inverted before it is used as a driver verifier.

**`step()` stages; `gate()` lands.** A driver that never gates loses nothing — every session is
committed on its own branch and the span tip holds them all — but the base branch never moves
either. A gate on an empty span is free, so calling it unconditionally at the bottom of a `for` is
the right default.

---

## Where to look next

| | |
|---|---|
| `examples/workflow.rs` | the full surface in use — steps, all three verifier kinds, `&&` gating, `block()` |
| `examples/selfimprove.rs` | a driver that improves the tool that runs it, and the hazards that come with it |
| `examples/workflow.yaml` | the same workflow in YAML, with an honest list of what it loses |
| `docs/CONFIG.md` | every `rlt.yaml` key |
