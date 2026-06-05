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

AgenticGoGo (`agg`) wraps your agent in a proper loop that runs **fresh Claude Code workers,
one after another, until your goals are actually met** — judged by scripts and LLMs, not by
vibes. It heartbeats, watchdogs hung sessions, summarizes progress in plain English, shows
you a live dashboard, and lets you steer it from your phone. You set the finish line; it runs
to it.

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

AgenticGoGo is the missing **finishing** layer. The insight (learned the hard way driving an
agent through a long, repetitive build): keep the loop **dumb and cheap** — a plain program
that relaunches fresh, context-free workers — and reserve the LLM for the actual work and the
judging. No context accumulation, no runaway cost, no babysitting.

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

- **Goals** come in three flavors — `binary` (done y/n), `percentage` (≥ a target),
  `cardinal` (N of M) — with **regression detection** (a goal that breaks again is loud) and
  **invariant guards** (“never ship a wrong result” can halt the loop).
- **Judges** decide if a goal is met. A **script** judge runs a command (test suite,
  benchmark, linter) → verdict. An **LLM** judge scores artifacts against a **rubric** →
  verdict. Both emit the same tiny JSON contract.
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
`AGG_VERSION=v0.0.2`, or choose the dir with `AGG_INSTALL_DIR=~/bin`. macOS + Linux x86_64;
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
> on its own. See [`DESIGN.md`](DESIGN.md) §12.

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

## Compatible with your stack

Inner workers run in your project, so your other Claude Code plugins and MCP servers — engram,
get-shit-done, graphify — **just work** alongside `agg`. Use them to plan; use `agg` to finish.

Full architecture and design notes live in [`DESIGN.md`](DESIGN.md).

## License

MIT
