<p align="center">
  <img src="logo.png" width="200" alt="AgenticGoGo — a pole-dancing robot that keeps your agent going going">
</p>

<h1 align="center">AgenticGoGo</h1>

<p align="center"><em>Stop typing “go go”. Let your coding agent finish the job.</em></p>

---

Are you constantly typing **“go go”**, **“continue”**, **“keep going”** to nudge your coding
agent through a long plan? Do even spec-driven approaches stall mid-flight, run out of
context, or quietly stop one step short — leaving you to babysit a terminal?

**Then the AgenticGoGo harness is for you.**

AgenticGoGo (`agg`) is a **[Ralph loop](https://ghuntley.com/ralph/)** — the technique, coined
by Geoffrey Huntley, of re-launching a **fresh** coding-agent session over and over until a
spec is actually fulfilled, with the **filesystem as the source of truth** between iterations
rather than a single, degrading conversation. There's a whole family of these tools
([snarktank/ralph](https://github.com/snarktank/ralph), Anthropic's
[`ralph-wiggum`](https://github.com/anthropics/claude-code/tree/main/plugins/ralph-wiggum)
plugin, [vercel-labs/ralph-loop-agent](https://github.com/vercel-labs/ralph-loop-agent),
Steve Yegge's [Gas Town](https://github.com/steveyegge/gastown), and more); `agg` is the
**operationally-hardened single-machine** one.

Concretely: `agg` runs **fresh Claude Code workers, one after another, until your goals are
actually met** — judged by scripts and LLMs, not by vibes. It heartbeats, watchdogs hung
sessions, summarizes progress in plain English, shows you a live dashboard, and lets you steer
it from your phone. You set the finish line; it runs to it.

> 🔍 **New to the Ralph loop, or wondering how `agg` differs from the other tools?** See
> **[COMPARISON.md](COMPARISON.md)** for a feature-by-feature breakdown.

```
┌─ AgenticGoGo · my-project · up 3h12m · stop: all_goals ─────────────────────┐
│ ██████████████████████████████ Goals 7/10  70%                              │
├─ Goals ─────────────────────────────────────────────────────────────────────┤
│ ✔ tests_pass        cardinal   42/42      ▲+5              judge:script      │
│ ◑ coverage          percentage 81/90%                      judge:script      │
│ ✔ no_regressions    binary     yes        (guard)          judge:script      │
│ ✖ code_quality      percentage 72/85%                      judge:llm:haiku   │
├─ Activity ──────────────────────────────────────────────────────────────────┤
│ session #7  running  idle 12s  tokens 2.1M / 5.0M                            │
│ now:   🔧 $ Run the test suite                                               │
│ think: implementing the remaining edge cases in the parser                  │
├─ Summary  (q to quit) ───────────────────────────────────────────────────────┤
│ story: Building the parser; tests green, coverage at 81%, quality lagging.   │
│ recent: Fixed the nested-group case; the suite is fully green now.           │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## Why it exists

A single agent session is **bounded** — by context, by attention, by the human in the loop.
For genuinely long work (a refactor, a benchmark chase, a milestone) you end up hand-cranking
it: *go… go… go…*. Spec tools help you **plan**, but planning isn't **finishing**.

The Ralph loop is the answer the field converged on: keep the loop **dumb and cheap** — a plain
program that relaunches fresh, context-free workers — and reserve the LLM for the actual work
and the judging. No context accumulation, no runaway cost, no babysitting. (Huntley's purest
form is literally `while :; do cat PROMPT.md | claude-code ; done`.) AgenticGoGo is the missing
**finishing** layer built on that idea — but where most Ralph implementations stop at the bare
loop, `agg` adds the parts you actually need to leave a multi-hour run unattended:

- a **typed multi-goal scoreboard** with **regression detection** (a met goal that breaks again
  is a first-class signal) and **invariant guards** that halt the moment the worker cheats;
- **LLM-as-judge** as a shipped feature, not a self-asserted "done";
- a **two-signal watchdog** (silent *and* CPU-flat), **rate-limit backoff**, and **process-group
  reaping** so a hung or runaway worker can't wedge the loop or leak compute;
- a **safe stop-condition language** (not `eval`), **per-session git isolation**, and **live +
  mobile steering**.

That combination is what distinguishes it from the lighter members of the family — see
[COMPARISON.md](COMPARISON.md). For *parallel-fleet* scale (20–30 agents at once) rather than a
hardened single-machine loop, look at Gas Town instead; `agg` is deliberately sequential.

## How it works

```
   you ── /agg:new ──►  goals.yaml + agg.yaml + AGG_RESUME.md
                              │
                              ▼
   agg run  ── loop ─────────────────────────────────────────────┐
     │  launch a FRESH `claude -p` worker  (one chunk of work)    │
     │  …stream it readably, heartbeat, watchdog a hung session   │
     │  on exit → run JUDGES → update GOALS → check STOP          │  repeat until
     │  every cycle → a 1-line progress SUMMARY                   │  goals met
     └────────────────────────────────────────────────────────────┘
            ▲ agg dashboard (live TUI)     ▲ agg send (steer it)
```

This is the canonical Ralph shape: a **dumb outer loop** (no tokens) wraps a **fresh-context
inner worker** (the real cost). Each session starts clean — Huntley's *"one context window, one
activity, one goal"* — so the loop never compacts or degrades; state carries across sessions via
the **filesystem** (your code, git, the resume prompt), not a growing conversation.

- **Goals** come in three flavors — `binary` (done y/n), `percentage` (≥ a target),
  `cardinal` (N of M) — with **regression detection** (a goal that breaks again is loud) and
  **invariant guards** (“never ship a wrong result” can halt the loop).
- **Judges** are the loop's **backpressure** — the verification gates that reject not-yet-good
  work so the next session redoes it. A **script** judge runs a command (test suite, benchmark,
  linter) → verdict. An **LLM** judge scores artifacts against a **rubric** → verdict (Ralph's
  prescribed answer for subjective criteria). Both emit the same tiny JSON contract.
- **Stop conditions** are a safe little expression language — `all_goals`,
  `met_fraction >= 0.75`, `count_met >= 3`, `goal_a OR goal_b`,
  `any_regressed(invariants) OR over_budget` — *not* `eval`.
- **Summaries** — one cheap call per cycle turns the worker's raw thoughts + the goal deltas
  into a *cumulative* “story so far” and a *windowed* “last cycle” line.
- **Watchdog** — kills a worker that's gone silent **and** CPU-flat (born from a real multi-hour worker
  hang), and auto-relaunches.
- **Steer it live** — `agg send inject "focus on X"` / `budget …` / `pause` / `stop`, applied
  at the next session boundary. Attach an outer `claude --remote-control` session running
  `/agg:supervise` and drive the whole thing **from your phone**.

## Install

You need the [Claude Code](https://claude.com/claude-code) CLI on your PATH (AgenticGoGo
drives it). Then get `agg` one of three ways:

**A) One-line install** (detects your OS/arch, downloads the right release binary, puts it on PATH):

```bash
curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/install.sh | sh
```

Installs to `/usr/local/bin` (or `~/.local/bin` if that's not writable). Pin a version with
`AGG_VERSION=v0.0.5`, or choose the dir with `AGG_INSTALL_DIR=~/bin`. macOS + Linux x86_64;
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

> Why two installs? `agg` runs in *your* terminal (it launches Claude workers); the `/agg:*`
> skills run *inside* Claude. A plugin can't put a binary on your shell PATH, so the CLI ships
> on its own.

## Fastest start: `agg init`

In any project directory:

```bash
agg init     # scaffolds goals.yaml + agg.yaml + AGG_RESUME.md + a starter judge
agg plan     # dry-run: shows the starting scoreboard
# edit the scaffolded files for your project, then:
agg run
```

`agg init` writes a runnable starter (with comments explaining each piece) so you're
never staring at a blank page. The walked example below shows what those files contain.

Stuck? **`agg doctor`** checks everything in one shot — claude on PATH, config parses,
stop/halt conditions valid, resume prompt present — and tells you exactly what to fix.

## Hello, agg — the smallest possible loop

The whole idea in four tiny files: a worker does a task, a **judge** (any script that prints
one line of JSON `{"met": …}`) checks it, and the loop repeats until the judge says met.

We start with a *broken* `add.py` so you can watch the correction loop happen.

```python
# add.py  — starts WRONG on purpose (prints 3, not 2)
print(1 + 1 + 1)
```
```bash
# check.sh  — the judge: print one JSON line. That's the whole contract.
#!/usr/bin/env bash
[ "$(python3 add.py 2>/dev/null)" = "2" ] \
  && echo '{"met":true}' \
  || echo '{"met":false,"rationale":"add.py did not print 2"}'
```
```
# AGG_RESUME.md  — the worker's standing instruction (one line)
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

The judge rejects `3`, the worker edits `add.py` to `print(1 + 1)`, the judge sees `2` →
`met:true` → the loop stops. That's the entire model. Everything below is just more goals,
smarter judges, and guardrails on top of this.

## Walked example: drive a project to "all tests pass"

Here's the whole thing end to end on a tiny project — a Python lib with three unimplemented
functions and a failing test suite. AgenticGoGo will keep launching Claude workers until the
tests pass, then stop.

**1. The project** (`calc.py` has stubs that raise `NotImplementedError`; `test_calc.py` tests them):

```python
# calc.py
def add(a, b):       raise NotImplementedError
def factorial(n):    raise NotImplementedError
def is_prime(n):     raise NotImplementedError
```

**2. A judge** — `judges/tests.sh` runs the suite and prints a verdict:

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

**3. `goals.yaml`** — one cardinal goal, met when all 3 tests pass; halt after 30 min:

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

**4. `agg.yaml`** — harness config + the worker's standing instructions file:

```yaml
project: calc
model: "claude-opus-4-8[1m]"
resume_prompt: "AGG_RESUME.md"
budget: { total: 2000000 }
summary: { enabled: true, model: haiku, min_interval_secs: 1 }
```

**5. `AGG_RESUME.md`** — the prompt fed to *every* fresh worker session:

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
agg plan        # dry run: shows "tests_pass cardinal 0/3 — loop would continue"
agg run         # launches real Claude workers until the tests pass, then stops
agg dashboard   # (optional, second terminal) live colored TUI
```

What happens: a fresh worker reads the prompt, runs pytest, implements the functions, re-runs
pytest → green, and exits. The loop runs the judge → goal flips `0/3 → 3/3` → **stop condition
met → loop exits after one session.** A one-line summary records what it did. *(This exact run
is part of the project's end-to-end test — the worker writes correct, idiomatic code.)*

### Or let `/agg:new` write it for you

In a project you've already planned (a PRD, ROADMAP, get-shit-done `.planning/`, or a README),
just run the skill inside Claude Code:

```
/agg:new        # reads your plans → writes goals.yaml + agg.yaml + AGG_RESUME.md
```

It **translates** whatever plan exists into goals + judges (it doesn't replicate your spec
tooling) and asks only about genuine gaps. Then exit Claude and `agg run`.

## Showcase: "prove P ≠ NP" — every feature on one famous problem

> ⚠️ **This loop will not solve P ≠ NP. That's the point.** It's a showcase of the full
> machinery on a hard, open-ended research problem — measurable *partial* progress, a
> mechanical check that **cannot be faked**, a soundness guard that halts on cheating, and a
> paper. Point the same structure at *your* research problem; swap the checker for your domain.

The trick that makes this honest rather than theatre: the proof is written in **[Lean 4](https://lean-lang.org/)**
— the formal proof assistant used by working mathematicians today (its `Mathlib` library is
the largest formal math corpus in existence, used from undergrad courses to Fields medalists).
Lean's kernel mechanically rejects any proof with a gap, so **"verified" literally cannot be
faked** — and `sorry` (Lean's "I gave up here" placeholder) is caught by a soundness invariant
that halts the loop. The worker won't finish P≠NP (nobody has), but it produces *verified
supporting lemmas* — real, checkable progress — and a paper. *(Needs the Lean toolchain;
wired in via an `on_start` hook.)*

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
halt_when: "not no_sorry"                         # stop instantly if it smuggles in a gap
```

**`judges/verify_proof.sh`** — the real, uncheatable check (Lean compiles, no `sorry`):
```bash
#!/usr/bin/env bash
out="$(lake build 2>&1)"; rc=$?
if [ $rc -eq 0 ] && ! grep -rq "sorry" proof/; then
  echo '{"met":true,"rationale":"Lean verified the full proof — no sorry"}'
else
  echo "{\"met\":false,\"rationale\":\"Lean does not accept the proof yet (rc=$rc)\"}"
fi
```

**`judges/no_sorry.sh`** — the soundness invariant (the anti-cheating showpiece):
```bash
#!/usr/bin/env bash
if grep -rqE '\bsorry\b|\badmit\b' proof/; then
  echo '{"met":false,"rationale":"a proof file contains sorry/admit — gap, not a proof"}'
else
  echo '{"met":true,"rationale":"no sorry/admit anywhere"}'
fi
```

**`agg.yaml`** — wires the Lean toolchain via a hook (agg stays tool-agnostic; *you* supply it):
```yaml
project: p-vs-np
model: "claude-opus-4-8[1m]"
resume_prompt: AGG_RESUME.md
budget: { total: 50000000 }              # a hard ceiling — this one could run forever
hooks:
  on_start: ["lake build || true"]        # fetch/build the Lean project + Mathlib once
summary: { enabled: true, model: haiku }
```

What you get even though the prize is unreachable: a steadily-growing count of
**Lean-verified lemmas** (real progress on the dashboard), a paper, a loop that **halts the
moment it tries to cheat** (`sorry`), and a hard token ceiling so a hopeless run can't bankrupt
you. That's the full feature set — cardinal/binary/LLM goals, script + LLM judges, invariants,
`halt_when`, `recheck`, hooks, budget — on a problem everyone recognizes.

## The judge contract

A judge is *any command that prints this JSON to stdout*:

```json
{"met": false, "value": 18, "max": 28, "target": 28, "rationale": "18/28 tests pass"}
```

`script` judges run a command; `llm` judges run a `claude -p` call against a rubric. Bundled
ones (`plugin/judges/`, `plugin/rubrics/`) cover common cases — `cargo_test`, `cmd_exit`,
`grep_count`, plus code-quality / docs / task-completion rubrics.

## Steering a running loop

You can't interrupt a headless worker mid-thought (a platform limit, by design), so steering
is **session-granular** — queued and applied at the next boundary:

```bash
agg send inject "focus on the auth module next; it's the blocker"
agg send budget 8000000        # change the token ceiling
agg send pause                 # hold; `agg send resume` to continue
agg stop "done for today"      # graceful stop at the next boundary (alias of `send stop`)
```

### Run it in the background

A long loop should outlive your terminal. `--detach` forks it off, writes a pidfile, and
logs to `.agg/run.log` — no `nohup` incantation to remember:

```bash
agg run --detach        # or: agg run -d
tail -f .agg/run.log    # follow it
agg dashboard           # …or watch the live TUI
agg stop                # graceful stop
```

## Tuning & extension

**Don't re-check a finished goal (`recheck:`)** — by default every goal's judge runs each
cycle. For a goal whose status can't change once achieved (a written report, a completed
study) that wastes work — especially with an LLM judge. Set a recheck policy in `goals.yaml`:

```yaml
- id: report_written
  recheck: once_met        # judge until first met, then LATCH — never re-judged (shown 🔒)
  judge: { kind: llm, model: haiku, rubric: "judges/report.md", inputs: ["REPORT.md"] }

- id: artifact_valid
  recheck: on_change       # re-judge only when a declared input changes (by content hash)
  recheck_inputs: ["build/out.json"]
  judge: { kind: script, cmd: "./judges/validate.sh" }
```

`always` (default) is required for invariants — their status can regress, so agg rejects
`once_met` on an `invariant: true` goal.

**Wire in your own tooling (generic hooks).** agg is tool-agnostic: it runs *your* shell
commands at lifecycle moments and prepends *your* text to the worker prompt. Use this for a
code-graph builder, a memory cache, a linter — whatever you use. Nothing is hardcoded.

```yaml
hooks:
  on_start:         ["mytool build ."]      # once at startup
  on_session_start: ["mytool refresh ."]    # before each worker session
  on_session_end:   ["mytool persist ."]    # after each session's judging
  on_stop:          ["mytool export ."]     # once when the loop stops
  background:       ["mytool --watch ."]    # long-lived; reaped automatically on stop
prompt_includes:
  - "AGG_TOOLING.md"                        # your text, prepended to every worker prompt
```

A failing hook is logged, never fatal. `background` processes are spawned in the loop's
reaping domain, so a `--watch` can't leak (see below).

**No orphaned compute.** The worker runs in its own process group; when a session ends (or
the loop stops), agg sweeps the whole group and kills any straggler — even a `nohup … &` or
`--watch` child that escaped. Works on Linux, macOS, and Windows (process-group / tree kill,
no fragile env-reading).

## Compatible with your stack

Inner workers run in your project, so your other Claude Code plugins and MCP servers — engram,
get-shit-done, graphify — **just work** alongside `agg`. Use them to plan; use `agg` to finish.

## Where AGG fits in the Ralph family

`agg` didn't invent the loop — it's one of many [Ralph-loop](https://ghuntley.com/ralph/)
implementations, and it stands on Huntley's pattern and the tools that came before it. What it
adds is **operational completeness for an unattended single-machine run**: typed goals +
regression detection + invariant halt-on-cheating + a safe stop-condition DSL (a combination no
other tool in the family has), plus a shipped LLM judge, a two-signal watchdog, rate-limit
backoff, process reaping, per-session git isolation, and mobile steering.

Quick orientation:

| Tool | Best at | `agg` vs it |
|---|---|---|
| [snarktank/ralph](https://github.com/snarktank/ralph) | The popular, simple PRD-driven loop | `agg` adds the safety/observability rails it lacks (watchdog, budgets, LLM judge, invariants, steering) |
| [`ralph-wiggum`](https://github.com/anthropics/claude-code/tree/main/plugins/ralph-wiggum) (Anthropic) | One-command in-session "keep going" | `agg` runs **fresh** sessions instead of one that compacts/degrades |
| [vercel-labs/ralph-loop-agent](https://github.com/vercel-labs/ralph-loop-agent) | AI-SDK lib with a `$` cost cap | `agg` is a real harness (watchdog, dashboard, git isolation); vercel has a literal dollar ceiling `agg` doesn't |
| [Gas Town](https://github.com/steveyegge/gastown) (Yegge) | **Parallel** 20–30-agent fleets | A different class — `agg` is the lighter, cheaper, single-operator sequential loop |

The full feature-by-feature matrix, honest gaps, and sources are in **[COMPARISON.md](COMPARISON.md)**.

## License

MIT
