<p align="center">
  <img src="assets/logo.png" width="200" alt="AgenticGoGo — a pole-dancing robot that keeps your agent going going">
</p>

<h1 align="center">AgenticGoGo</h1>

<p align="center"><em>A deterministic outer loop around a stochastic agent. Stop typing “go go”.</em></p>

---

Are you constantly typing **“go go”**, **“continue”**, **“keep going”** to nudge your coding
agent through a long plan? Do even spec-driven approaches stall mid-flight, run out of context,
or quietly stop one step short — leaving you to babysit a terminal?

**Then the AgenticGoGo harness is for you.**

AgenticGoGo (`agg`) is a **deterministic outer loop** that drives a **stochastic inner agent**
(`claude -p`) — relaunching it, verifying its work against gates *it can't fake*, and repeating
until your goals are actually met. It's a hardened take on the **[Ralph loop](https://ghuntley.com/ralph/)**
(Geoffrey Huntley's technique of re-launching a **fresh** agent session over and over, with the
**filesystem as the source of truth** between iterations rather than one degrading conversation).

The whole design is one sentence: **keep the loop dumb and deterministic; keep the model in a box.**
The loop is plain code — it never hallucinates a decision. The model does the work, inside one
step, and never decides when it's done. That split is what lets `agg` run a probabilistic agent
**safely, on repeat, unattended, until the work is genuinely finished.**

> 🔍 **New to the Ralph loop, or wondering how `agg` differs from the other tools?** See
> **[COMPARISON.md](COMPARISON.md)** for a feature-by-feature breakdown.

<p align="center">
  <img src="assets/dashboard.png" alt="The agg dashboard — a live TUI showing the Info, Progress, Goals, Activity, and Summary panels for a running loop" width="820">
</p>

<p align="center"><sub><code>agg dashboard</code> — the live TUI (rendered from the real dashboard code; regenerate with <code>cargo run --example dashboard_svg</code>).</sub></p>

---

## The two loops

Everything about `agg` follows from separating two loops that most tooling tangles together.

**The outer loop is `agg`. It is deterministic.** Plain Rust, no model in the control path —
given the same state and the same verdicts, the same code path always runs. Four stages a cycle:

```
INJECT  state + steering → the agent's prompt   (resume prompt + AGG_MEMORY.md + your bus commands)
RUN     one fresh `claude -p` agent             ← the ONE stochastic step: an opaque black box
VERIFY  agg runs the judges itself, externally, against the filesystem
GATE    keep or roll back · check stop/halt · carry state forward → repeat
```

**The inner loop is the agent, and it is stochastic.** Whatever it does inside `RUN` — plan, act,
observe, reason — is *its* business. Pick whatever inner-loop framework you like for that part:
**ReAct**, NVIDIA's **Context–Observe–Reason–Act**, a **DISCOVER→PLAN→EXECUTE** cycle. `agg`
neither sees nor cares. To the outer loop, `RUN` is a single opaque, probabilistic step.

That's what *"keep the model out of the loop"* actually means: the LLM lives inside `RUN` only.
`INJECT`, `VERIFY`, and `GATE` are code. **The one thing every inner-loop framework leaves to the
agent — deciding whether the work is good — `agg` takes away from it.** A deterministic outer loop
is only trustworthy if `VERIFY` is deterministic too: judges that **execute against the
filesystem**, never the agent grading its own homework. That is the moat, and it is the reason a
loop finishes work instead of spinning.

Those inner-loop frameworks describe how *one agent thinks*. `agg` is the machine that runs a
thinking agent **on repeat, and stops it lying about being done.**

## What each stage actually does

The four outer-loop stages, concretely — this is the whole product.

### `INJECT` — hand the agent the goal and everything it's allowed to know
Each cycle starts fresh (Huntley's *"one context window, one activity, one goal"*), so nothing
compacts or degrades. Deterministic state carries **across** sessions via the filesystem, not a
growing chat:

- **The resume prompt** — the standing instruction fed to every fresh agent.
- **Institutional memory** — a durable, committable `AGG_MEMORY.md` of rolled-up learnings, read
  back into every fresh agent plus an always-on “last session” block, so a brand-new context
  isn't amnesiac. On by default, zero setup. `agg` **enforces the write itself** after every
  session (a folded agent note if present, else the windowed summary, else the mechanical facts) —
  it never trusts a crashed or skipping agent. Two independent caps keep it cheap: `inject_kb`
  bounds per-prompt injection, `max_kb` bounds the file on disk (oldest entries drop first).
- **Your steering** — anything you queued on the bus (`inject` / `budget` / `pause` / …), applied
  at this boundary.

### `RUN` — the stochastic step, in a box
One fresh `claude -p` agent, full host access to do the work, streamed to a readable log with a
**heartbeat** and a **two-signal watchdog** (silent *and* CPU-flat — born from a real multi-hour
hang) that kills a wedged agent and relaunches. This is the only non-deterministic part of the
whole system, and it is deliberately opaque: `agg` treats the agent's internal reasoning as a
black box and judges only its *output*.

### `VERIFY` — external, unfakeable gates (the moat)
When the agent exits, **`agg`** runs the checks — the agent does not grade itself.

- **Judges** are the loop's **backpressure**: verification gates that reject not-yet-good work so
  the next cycle redoes it. A **script** judge runs a command (test suite, benchmark, linter) → a
  verdict. An **LLM** judge scores artifacts against a **rubric** → a verdict (Ralph's prescribed
  answer for subjective criteria). Both emit the same tiny JSON contract, and both run *outside*
  the agent's context.
- **Goals** come in three flavors — `binary` (done y/n), `percentage` (≥ a target), `cardinal`
  (N of M) — with **regression detection** (a met goal that breaks again is a loud, first-class
  signal) and **invariant guards** (“never ship a wrong result” can halt the loop the instant the
  agent cheats).

### `GATE` — decide, deterministically, then loop or stop
Pure code deciding what happens next:

- **Rollback gate** — with per-session git isolation on, the merge is *staged* so judges test the
  merged tree; a regression **rolls it back** (base stays pristine, branch kept for inspection),
  otherwise it commits. Base never regresses.
- **Stop conditions** — a safe little expression language (`all_goals`, `met_fraction >= 0.75`,
  `count_met >= 3`, `goal_a OR goal_b`, `any_regressed(invariants) OR over_budget`) — *not*
  `eval`. Three ceiling guards stop a runaway: **`over_budget`** (tokens), **`over_cost`**
  (dollars — API-equivalent usage Claude reports, *not* a subscription charge; see the note below),
  **`over_iterations`** (sessions). OR them together and the loop halts the moment any one trips.
- **Carry state forward** — fold the session into `AGG_MEMORY.md`, write a one-line summary
  (a cheap per-cycle call turning the agent's raw thoughts + goal deltas into a *cumulative* “story
  so far” and a *windowed* “last cycle” line), then loop.

### The whole cycle, in one picture

<p align="center">
  <img src="assets/how-it-works.png" alt="How agg works: /agg:new creates goals.yaml + agg.yaml + AGG_RESUME.md; a baseline judge runs once; then the deterministic agg run outer loop, each cycle — INJECT (drain the steering bus, build the prompt from memory), RUN (a fresh stochastic claude -p worker with heartbeat + watchdog), VERIFY (rate-limit check, stage the merge under git isolation, run judges on the filesystem and update goals), GATE (rollback gate, fold memory + summarize, check STOP/HALT) — repeating until goals are met or a guard halts. agg dashboard, agg send/stop, and agg spawn sit alongside." width="960">
</p>

<p align="center"><sub>The deterministic outer loop; <code>RUN</code> is the single stochastic step. (Regenerate with <code>cargo run --example how_it_works_svg</code>.)</sub></p>

## Why it exists

A single agent session is **bounded** — by context, by attention, by the human in the loop. For
genuinely long work (a refactor, a benchmark chase, a milestone) you become the loop: *go… go…
go…*. Spec tools help the agent **plan**; they don't make it **finish**.

The Ralph insight is to invert that: don't sit inside the agent pushing it step by step — wrap it
in a dumb, cheap program that relaunches fresh, context-free agents and reserves the model for the
work and the judging. (Huntley's purest form is literally
`while :; do cat PROMPT.md | claude-code ; done`.) `agg` is that program, hardened into the parts
you actually need to walk away from a multi-hour run — every one of them living in the
**deterministic** `INJECT`/`VERIFY`/`GATE` stages, never in the agent:

- a **typed multi-goal scoreboard** with **regression detection** and **invariant guards**;
- **LLM-as-judge** as a shipped, external gate — not a self-asserted “done”;
- a **two-signal watchdog**, **rate-limit backoff**, and **process-group reaping** so a hung or
  runaway agent can't wedge the loop or leak compute;
- a **safe stop-condition language** (not `eval`), **per-session git isolation** with a rollback
  gate, and **live + mobile steering**.

For *parallel-fleet* scale (20–30 agents at once) rather than a hardened single-machine loop, look
at [Gas Town](https://github.com/steveyegge/gastown); `agg` is deliberately sequential.

## Install

You need the [Claude Code](https://claude.com/claude-code) CLI on your PATH (`agg` drives it as
the inner agent). Then get `agg` one of three ways:

**A) One-line install** (detects your OS/arch, downloads the right release binary, puts it on PATH):

```bash
curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/install.sh | sh
```

Installs to `/usr/local/bin` (or `~/.local/bin` if that's not writable). Pin a version with
`AGG_VERSION=v0.0.11`, or choose the dir with `AGG_INSTALL_DIR=~/bin`. macOS + Linux x86_64;
Windows users grab the `.exe` from Releases (option B).

**B) Download a prebuilt binary** (from [Releases](https://github.com/ssenge/AgenticGoGo/releases)):

```bash
# macOS (Apple Silicon) — adjust the asset name for your platform
curl -L -o agg https://github.com/ssenge/AgenticGoGo/releases/latest/download/agg-aarch64-apple-darwin
chmod +x agg && sudo mv agg /usr/local/bin/
agg --version
```

**C) Build from source** (needs the Rust toolchain):

```bash
git clone https://github.com/ssenge/AgenticGoGo.git
cd AgenticGoGo
cargo build --release
sudo cp target/release/agg /usr/local/bin/   # or add target/release to your PATH
agg --version
```

**The plugin** (the `/agg:*` skills) installs separately, inside Claude Code:

```
/plugin marketplace add ssenge/AgenticGoGo
/plugin install agg@agenticgogo
```

> Why two installs? `agg` (the outer loop) runs in *your* terminal — it launches the inner agents;
> the `/agg:*` skills run *inside* Claude. A plugin can't put a binary on your shell PATH, so the
> CLI ships on its own.

## Fastest start: `agg init`

In any project directory:

```bash
agg init     # scaffolds goals.yaml + agg.yaml + AGG_RESUME.md + a starter judge
agg plan     # dry-run: shows the starting scoreboard (one VERIFY pass, no RUN)
# edit the scaffolded files for your project, then:
agg run
```

`agg init` writes a runnable starter (with comments explaining each piece) so you're never staring
at a blank page. The walked example below shows what those files contain.

**Prefer a tidy root?** `agg init --folder` puts everything in an optional **`agg/` config folder**
(`agg/agg.yaml`, `agg/goals.yaml`, `agg/AGG_RESUME.md`, `agg/judges/`, `agg/rubrics/`) instead of
the project root. `agg run` auto-detects either layout — no flag needed at run time. Inside the
folder, the resume prompt and rubric files resolve relative to `agg/`; judge `cmd`s still run from
the project root (so a foldered judge is `cmd: "./agg/judges/x.sh"`). Runtime state always lives in
`.agg/` (note the dot) regardless.

Stuck? **`agg doctor`** checks everything in one shot — claude on PATH, config parses, stop/halt
conditions valid, resume prompt + rubric files present, and which config layout it found — and
tells you exactly what to fix.

## Hello, agg — the smallest possible loop

The whole idea in four tiny files. `INJECT` hands the agent a one-line task; `RUN` lets it edit;
`VERIFY` is a **judge** (any script that prints one line of JSON `{"met": …}`); `GATE` repeats
until the judge says met.

We start with a *broken* `add.py` so you can watch the correction loop happen.

```python
# add.py  — starts WRONG on purpose (prints 3, not 2)
print(1 + 1 + 1)
```
```bash
# check.sh  — the judge (VERIFY): print one JSON line. That's the whole contract.
#!/usr/bin/env bash
[ "$(python3 add.py 2>/dev/null)" = "2" ] \
  && echo '{"met":true}' \
  || echo '{"met":false,"rationale":"add.py did not print 2"}'
```
```
# AGG_RESUME.md  — the standing instruction INJECTed into every session (one line)
Fix add.py so that running `python3 add.py` prints exactly: 2
```
```yaml
# goals.yaml
goals:
  - id: prints_two
    type: binary
    judge: { kind: script, cmd: "./check.sh" }
stop_when: prints_two
```
```yaml
# agg.yaml
project: hello-agg
model: claude-haiku-4-5-20251001     # haiku — this costs next to nothing to try
resume_prompt: AGG_RESUME.md
```

```bash
chmod +x check.sh
agg run
```

`VERIFY` rejects `3`; the agent edits `add.py` to `print(1 + 1)`; `VERIFY` sees `2` → `met:true` →
`GATE` sees the stop condition met → the loop stops. That's the entire model. Everything below is
just more goals, smarter judges, and guardrails on the same four stages.

## Walked example: drive a project to "all tests pass"

The whole thing end to end on a tiny project — a Python lib with three unimplemented functions
and a failing test suite. `agg` keeps `RUN`ning fresh agents until `VERIFY` goes green, then stops.

**1. The project** (`calc.py` has stubs that raise `NotImplementedError`; `test_calc.py` tests them):

```python
# calc.py
def add(a, b):       raise NotImplementedError
def factorial(n):    raise NotImplementedError
def is_prime(n):     raise NotImplementedError
```

**2. A judge** (`VERIFY`) — `judges/tests.sh` runs the suite and prints a verdict:

```bash
#!/usr/bin/env bash
out="$(python3 -m pytest -q 2>&1)"
passed=$(printf '%s' "$out" | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+' || echo 0)
failed=$(printf '%s' "$out" | grep -oE '[0-9]+ failed' | grep -oE '[0-9]+' || echo 0)
total=$(( ${passed:-0} + ${failed:-0} ))
met=$([ "${failed:-0}" -eq 0 ] && [ "$total" -gt 0 ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s/%s tests pass"}\n' \
  "$met" "${passed:-0}" "$total" "$total" "${passed:-0}" "$total"
```

**3. `goals.yaml`** — one cardinal goal, met when all 3 tests pass; the `GATE` halts after 30 min:

```yaml
goals:
  - id: tests_pass
    type: cardinal
    target: 3
    description: "All calc tests pass"
    judge: { kind: script, cmd: "./judges/tests.sh", timeout: 60 }
stop_when: "tests_pass"
halt_when: "wall_hours >= 0.5"
```

**4. `agg.yaml`** — outer-loop config + the resume prompt `INJECT`ed into each session:

```yaml
project: calc
model: "claude-opus-4-8[1m]"
resume_prompt: "AGG_RESUME.md"
budget: { total: 2000000 }       # token ceiling  → over_budget  (a GATE guard)
cost:   { total: 5.0 }           # $ ceiling → over_cost (API-equivalent price Claude reports;
                                 #   a usage proxy, NOT a subscription charge — see note below)
summary: { enabled: true, model: haiku, min_interval_secs: 1 }
memory: { enabled: true, max_kb: 64, inject_kb: 8 }   # durable AGG_MEMORY.md, on by default
```

> **A note on `cost` / `over_cost`.** The dollar figure is `total_cost_usd` as reported by the
> `claude` CLI — the **API-equivalent list price** of the work. On a **Max/Pro subscription you are
> not billed per token**, so this is a **usage proxy, not money charged to you**; the dashboard and
> `agg status` label it `(API-eq)` for that reason. It's still a useful ceiling (`over_cost` halts a
> runaway loop by relative spend), but read it as "how much work" not "how much money" unless you're
> actually on pay-as-you-go API billing. Prefer `over_budget` (tokens) or `over_iterations` if you
> want a plan-agnostic cap.

**5. `AGG_RESUME.md`** — the prompt `INJECT`ed into *every* fresh session:

```
GOAL: make all tests in test_calc.py pass.
calc.py has add(a,b), factorial(n), is_prime(n) stubbed with NotImplementedError.

THIS SESSION:
1. Run `python3 -m pytest -q` to see what's failing.
2. Implement the failing function(s) in calc.py — real, correct implementations.
3. Re-run pytest to confirm. You are autonomous; do the work and exit.
```

**6. Run it:**

```bash
agg plan        # dry run: one VERIFY pass, no RUN — shows "tests_pass cardinal 0/3 — loop would continue"
agg run         # the outer loop: RUN real agents until VERIFY passes, then GATE stops
agg dashboard   # (optional, second terminal) live colored TUI
```

What happens: a fresh agent reads the injected prompt, runs pytest, implements the functions,
re-runs pytest → green, exits. `VERIFY` flips the goal `0/3 → 3/3`; `GATE` sees the stop condition
met → the loop exits after one session. A one-line summary records what it did.

### Or let `/agg:new` write it for you

In a project you've already planned (a PRD, ROADMAP, get-shit-done `.planning/`, or a README),
just run the skill inside Claude Code:

```
/agg:new        # reads your plans → writes goals.yaml + agg.yaml + AGG_RESUME.md
```

It **translates** whatever plan exists into goals + judges (it doesn't replicate your spec tooling)
and asks only about genuine gaps. Then exit Claude and `agg run`.

## Showcase: "prove P ≠ NP" — every feature on one famous problem

> ⚠️ **This loop will not solve P ≠ NP. That's the point.** It's a showcase of the full machinery
> on a hard, open-ended research problem — measurable *partial* progress, a `VERIFY` gate that
> **cannot be faked**, an invariant that halts on cheating, and a paper. Point the same structure
> at *your* research problem; swap the checker for your domain.

The trick that makes this honest rather than theatre is putting a truly deterministic checker in
`VERIFY`: the proof is written in **[Lean 4](https://lean-lang.org/)**, the formal proof assistant
mathematicians use today (its `Mathlib` is the largest formal-math corpus in existence). Lean's
kernel mechanically rejects any proof with a gap, so **"verified" literally cannot be faked** — and
`sorry` (Lean's "I gave up here" placeholder) is caught by a soundness invariant that halts the
loop. The agent won't finish P≠NP (nobody has), but it produces *verified supporting lemmas* — real,
checkable progress — and a paper. *(Needs the Lean toolchain; wired in via an `on_start` hook.)*

**`goals.yaml`** — multiple goal types, an LLM judge, invariants, sticky re-checking:
```yaml
goals:
  - id: proof_verified            # the (unreachable) prize: Lean checks the full proof
    type: binary
    judge: { kind: script, cmd: "./judges/verify_proof.sh", timeout: 1800 }

  - id: lemmas_verified           # MEASURABLE PARTIAL PROGRESS: N Lean-checked lemmas
    type: cardinal
    target: 20
    judge: { kind: script, cmd: "./judges/count_lemmas.sh", timeout: 1800 }

  - id: paper_written             # qualitative → an LLM (haiku) judge with a rubric
    type: binary
    recheck: once_met             # latch it: don't re-judge the paper every cycle
    judge: { kind: llm, model: haiku, rubric: "rubrics/paper.md", inputs: ["PAPER.md"] }

  - id: no_sorry                  # SOUNDNESS GUARD: no `sorry`/`admit`/stray axiom — ever
    type: binary
    invariant: true
    judge: { kind: script, cmd: "./judges/no_sorry.sh" }

stop_when: "proof_verified AND paper_written"     # the prize (don't hold your breath)
halt_when: "not no_sorry"                         # GATE stops instantly if it smuggles in a gap
```

**`judges/verify_proof.sh`** — the real, unfakeable `VERIFY` (Lean compiles, no gaps, no smuggled axioms):
```bash
#!/usr/bin/env bash
# lake build must succeed AND the soundness check (no sorry / axiom / native_decide) must pass.
here="$(cd "$(dirname "$0")" && pwd)"
out="$(lake build 2>&1)"; rc=$?
sound="$("$here/no_sorry.sh")"
if [ $rc -eq 0 ] && printf '%s' "$sound" | grep -q '"met":true'; then
  echo '{"met":true,"rationale":"Lean built the full proof and the soundness check passed"}'
else
  echo "{\"met\":false,\"rationale\":\"Lean does not accept the proof yet (rc=$rc)\"}"
fi
```

**`agg.yaml`** — wires the Lean toolchain via a hook (`agg` stays tool-agnostic; *you* supply it):
```yaml
project: p-vs-np
model: "claude-opus-4-8[1m]"
resume_prompt: AGG_RESUME.md
budget: { total: 50000000 }              # a hard GATE ceiling — this one could run forever
hooks:
  on_start: ["lake build || true"]        # fetch/build the Lean project + Mathlib once
summary: { enabled: true, model: haiku }
```

What you get even though the prize is unreachable: a steadily-growing count of **Lean-verified
lemmas** (real progress on the dashboard), a paper, a loop that **halts the moment it tries to
cheat**, and a hard token ceiling so a hopeless run can't bankrupt you. The full feature set —
every goal type, script + LLM judges, invariants, `halt_when`, `recheck`, hooks, budget — on a
problem everyone recognizes.

## The judge contract (`VERIFY`)

A judge is *any command that prints this JSON to stdout*:

```json
{"met": false, "value": 18, "max": 28, "target": 28, "rationale": "18/28 tests pass"}
```

`script` judges run a command; `llm` judges run a `claude -p` call against a rubric — in a fresh
session that loads only *your* settings, never the agent-mutated repo's `.claude/` config, so the
agent can't steer its own judge. Bundled ones (`plugin/judges/`, `plugin/rubrics/`) cover common
cases — `cargo_test`, `cmd_exit`, `grep_count`, plus code-quality / docs / task-completion rubrics.

## Steering a running loop (`INJECT`)

You can't interrupt a headless agent mid-thought (a platform limit, by design), so steering is
**session-granular** — queued on the bus and `INJECT`ed at the next boundary:

```bash
agg send inject "focus on the auth module next; it's the blocker"
agg send budget 8000000        # change the token ceiling
agg send pause                 # hold; `agg send resume` to continue
agg stop "done for today"      # graceful stop at the next GATE (alias of `send stop`)
```

### Run it in the background

A long loop should outlive your terminal. `--detach` forks it off, writes a pidfile, and logs to
`.agg/run.log` — no `nohup` incantation to remember. A `Ctrl-C` or `agg stop` shuts the agent and
the loop down cleanly (no orphaned worker, ledger finalized):

```bash
agg run --detach        # or: agg run -d
tail -f .agg/run.log    # follow it
agg dashboard           # …or watch the live TUI
agg stop                # graceful stop
```

### Watch it in the browser

A **standalone web UI** (SvelteKit, in [`web/`](web/)) monitors and steers a run from a browser —
locally now, deployable (e.g. Vercel) later. It talks to `agg` over HTTP; the agg binary stays
UI-free and just exposes a thin JSON API:

```bash
agg serve                          # thin JSON API on :7878 (state / history / health / send)
cd web && npm install && npm run dev   # the web tool on :5173
```

## Tuning & extension

**Don't re-check a finished goal (`recheck:`)** — by default `VERIFY` runs every goal's judge each
cycle. For a goal whose status can't change once achieved (a written report, a completed study)
that wastes work — especially with an LLM judge. Set a recheck policy in `goals.yaml`:

```yaml
- id: report_written
  recheck: once_met        # judge until first met, then LATCH — never re-judged (shown 🔒)
  judge: { kind: llm, model: haiku, rubric: "judges/report.md", inputs: ["REPORT.md"] }

- id: artifact_valid
  recheck: on_change       # re-judge only when a declared input changes (by content hash)
  recheck_inputs: ["build/out.json"]
  judge: { kind: script, cmd: "./judges/validate.sh" }
```

`always` (default) is required for invariants — their status can regress, so `agg` rejects
`once_met` on an `invariant: true` goal.

**Wire in your own tooling (generic hooks).** `agg` is tool-agnostic: it runs *your* shell commands
at lifecycle moments and prepends *your* text to the `INJECT`ed prompt. Use this for a code-graph
builder, a memory cache, a linter — whatever you use. Nothing is hardcoded.

```yaml
hooks:
  on_start:         ["mytool build ."]      # once at startup
  on_session_start: ["mytool refresh ."]    # before each RUN
  on_session_end:   ["mytool persist ."]    # after each VERIFY
  on_stop:          ["mytool export ."]     # once when the loop stops
  background:       ["mytool --watch ."]    # long-lived; reaped automatically on stop
prompt_includes:
  - "AGG_TOOLING.md"                        # your text, prepended to every agent prompt
```

A failing hook is logged, never fatal. `background` processes are spawned in the loop's reaping
domain, so a `--watch` can't leak (see below).

**What the agent can do — and constraining it.** `RUN` launches
`claude -p --dangerously-skip-permissions`: a headless `-p` agent can't answer permission prompts,
so it needs full tool access to make progress — which means **the agent runs with your user's full
host access**. The outer loop's rails (watchdog, budget/cost ceilings, git isolation, the rollback
gate) guard the *loop*; they do not sandbox the agent itself. For unattended overnight runs, prefer
running `agg` in a container/VM you're willing to hand to an autonomous agent.

To narrow what the agent may do, pass extra `claude` flags via `worker_args` in `agg.yaml`:

```yaml
worker_args: ["--allowedTools", "Edit,Bash", "--add-dir", "src"]   # or --disallowedTools, etc.
```

These are appended to every `RUN` (the judge and summarizer sessions run separately, *without*
`--dangerously-skip-permissions`, and load only your settings — never the agent-mutated repo's
`.claude/` config).

**No orphaned compute** *(macOS + Linux)*. The agent runs in its own process group; when a session
ends, the loop stops, or you `Ctrl-C`, `agg` sweeps the whole group and kills any straggler — even a
`nohup … &` or `--watch` child that escaped (POSIX process groups, no fragile env-reading).

> **Platform note.** `agg` is **unix-first** (macOS + Linux). The Windows binary builds and the
> **core outer loop runs** (INJECT → RUN → VERIFY → GATE, steering, dashboard), but two safety
> features are **not** implemented there: the **CPU-flat half of the watchdog** (a wedged agent is
> caught only by `over_iterations` / `agg stop`) and **process-group reaping**. `agg run` prints a
> one-line notice on Windows so this is never a surprise.

## Compatible with your stack

The inner agent runs in your project, so your other Claude Code plugins and MCP servers — engram,
get-shit-done, graphify — **just work** inside `RUN`. Use them to plan; use `agg` to finish.

## Where AGG fits in the Ralph family

`agg` didn't invent the loop — it's one of many [Ralph-loop](https://ghuntley.com/ralph/)
implementations, and it stands on Huntley's pattern and the tools before it. What it adds is
**making the outer loop deterministic and the `VERIFY` gate unfakeable**: typed goals + regression
detection + invariant halt-on-cheating + a safe stop-condition DSL (a combination no other tool in
the family has), plus a shipped LLM judge, a two-signal watchdog, rate-limit backoff, process
reaping, per-session git isolation with a rollback gate, and mobile + web steering.

| Tool | Best at | `agg` vs it |
|---|---|---|
| [snarktank/ralph](https://github.com/snarktank/ralph) | The popular, simple PRD-driven loop | `agg` adds the deterministic rails it lacks (watchdog, budgets, LLM judge, invariants, rollback, steering) |
| [`ralph-wiggum`](https://github.com/anthropics/claude-code/tree/main/plugins/ralph-wiggum) (Anthropic) | One-command in-session "keep going" | `agg` `RUN`s **fresh** sessions instead of one that compacts/degrades |
| [vercel-labs/ralph-loop-agent](https://github.com/vercel-labs/ralph-loop-agent) | AI-SDK lib with a `$` cost cap | `agg` is a real harness (watchdog, dashboard, git isolation) and also has the dollar ceiling (`cost.total` → `over_cost`) |
| [Gas Town](https://github.com/steveyegge/gastown) (Yegge) | **Parallel** 20–30-agent fleets | A different class — `agg` is the lighter, single-operator sequential loop |

The full feature-by-feature matrix, honest gaps, and sources are in **[COMPARISON.md](COMPARISON.md)**.

## License

MIT
