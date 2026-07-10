<p align="center">
  <img src="assets/logo.png" width="200" alt="AgenticGoGo — a pole-dancing robot that keeps your agent going going">
</p>

<h1 align="center">AgenticGoGo</h1>

<p align="center"><em>A deterministic outer loop around a stochastic agent. Stop typing “go go”.</em></p>

---

Are you constantly typing **“go go”**, **“continue”**, **“keep going”** to nudge your coding agent
through a long plan? Do even spec-driven approaches stall mid-flight, run out of context, or
quietly stop one step short — leaving you to babysit a terminal?

**Then the AgenticGoGo harness is for you.**

AgenticGoGo (`agg`) is a **deterministic outer loop** that drives a **stochastic inner agent**
(`claude -p`) — relaunching it, verifying its work against gates *it can't fake*, and repeating
until your goals are actually met. It's a hardened take on the
**[Ralph loop](https://ghuntley.com/ralph/)** (Geoffrey Huntley's technique of re-launching a
**fresh** agent session over and over, with the **filesystem as the source of truth** between
iterations rather than one degrading conversation).

The whole design is one sentence: **keep the loop dumb and deterministic; keep the model in a box.**
The loop is plain code — it never hallucinates a decision. The model does the work, inside one
step, and never decides when it's done.

## The loop

<p align="center">
  <img src="assets/loop.png" alt="The four stages of the agg loop — INJECT, RUN, VERIFY, GATE — arranged in a circle" width="620">
</p>

| Stage | What it does | Who runs it |
|---|---|---|
| **`INJECT`** | Builds the agent's prompt: your standing instruction, what past sessions learned, any steering you queued. | code |
| **`RUN`** | Launches one **fresh** `claude -p` session. It edits files. It never decides whether it succeeded. | **the model** |
| **`VERIFY`** | `agg` runs your **judges** itself, against the filesystem. The agent is never asked to grade its own homework. | code |
| **`GATE`** | Keeps or rolls back the work, checks `stop_when` / `halt_when`, carries state forward — or stops. | code |

Three of the four stages are plain Rust. Only `RUN` is stochastic. That split is what lets `agg`
run a probabilistic agent **safely, on repeat, unattended, until the work is genuinely finished** —
and it's why `VERIFY` is the moat: a gate the worker cannot fake, because the worker never runs it.

<p align="center">
  <img src="assets/dashboard.png" alt="The agg dashboard — a live TUI showing Info, Progress, Goals, Activity and Summary panels" width="820">
</p>
<p align="center"><sub><code>agg dashboard</code> — the live TUI (rendered from the real dashboard code).</sub></p>

## Install

You need the [Claude Code](https://claude.com/claude-code) CLI on your PATH — `agg` drives it as
the inner agent. Your other Claude Code plugins and MCP servers keep working inside `RUN`.

```bash
# A) one-liner (detects OS/arch; installs to /usr/local/bin, or ~/.local/bin if that's read-only)
curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/install.sh | sh

# B) prebuilt binary — see Releases (Windows: take the .exe)
# C) from source (needs the Rust toolchain)
cargo build --release && sudo cp target/release/agg /usr/local/bin/
```

Pin a version with `AGG_VERSION=v0.0.11`; choose the directory with `AGG_INSTALL_DIR=~/bin`.

The `/agg:*` skills are a **separate** install, inside Claude Code — a plugin can't put a binary on
your shell PATH:

```
/plugin marketplace add ssenge/AgenticGoGo
/plugin install agg@agenticgogo
```

## Quick start

```bash
agg init     # scaffolds goals.yaml + agg.yaml + AGG_RESUME.md + a starter judge
agg plan     # dry run: one VERIFY pass, prints the scoreboard. No agent is launched.
agg run      # drive it until stop_when is met
```

`agg init --folder` keeps your root tidy by scaffolding into `agg/` instead; `agg run` auto-detects
either layout. Stuck? **`agg doctor`** checks claude-on-PATH, config parsing, condition syntax and
missing files in one shot, and tells you what to fix.

### Hello, agg — the smallest possible loop

Four tiny files. We start with a **broken** `add.py`, so you can watch the correction loop happen.

```python
# add.py — wrong on purpose (prints 3, not 2)
print(1 + 1 + 1)
```

```bash
# check.sh — the judge (VERIFY). Print one line of JSON. That is the whole contract.
#!/usr/bin/env bash
[ "$(python3 add.py 2>/dev/null)" = "2" ] \
  && echo '{"met":true}' \
  || echo '{"met":false,"rationale":"add.py did not print 2"}'
```

```
# AGG_RESUME.md — the standing instruction INJECTed into every session
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
model: claude-haiku-4-5-20251001   # haiku — costs next to nothing to try
resume_prompt: AGG_RESUME.md
```

```bash
chmod +x check.sh && agg run
```

`VERIFY` rejects `3` → the agent edits `add.py` → `VERIFY` sees `2` → `GATE` sees `stop_when` met →
the loop stops. That's the entire model. Everything else is more goals, smarter judges, and
guardrails on the same four stages.

> Runnable, plus a longer walkthrough that drives a project to "all tests pass":
> **[`examples/hello-agg/`](examples/hello-agg/)**

## The judge contract (`VERIFY`)

A judge is *any command that prints this JSON to stdout*:

```json
{"met": false, "value": 18, "max": 28, "target": 28, "rationale": "18/28 tests pass"}
```

`script` judges run a command. `llm` judges run a `claude -p` call against a rubric, in a fresh
session that loads only **your** settings — never the agent-mutated repo's `.claude/` config — so
the agent can't steer its own judge. Bundled judges and rubrics live in
[`plugin/judges/`](plugin/judges/) and [`plugin/rubrics/`](plugin/rubrics/): `cargo_test`,
`cmd_exit`, `grep_count`, plus code-quality / docs / task-completion rubrics.

## Steering a running loop

You can't interrupt a headless agent mid-thought (a platform limit), so steering is
**session-granular**: queued on a bus, applied at the next `INJECT`.

```bash
agg run --detach               # fork it off; logs to .agg/run.log
agg dashboard                  # the live TUI (q to quit)
agg send inject "focus on the auth module; it's the blocker"
agg send budget 8000000        # change the token ceiling mid-run
agg send pause                 # …and `agg send resume`
agg stop "done for today"      # graceful stop at the next GATE
```

`Ctrl-C` or `agg stop` shuts the agent and the loop down cleanly — no orphaned worker, ledger
finalized, base branch untouched.

A standalone **web UI** (SvelteKit, in [`web/`](web/)) monitors and steers a run from a browser.
The binary stays UI-free and exposes a thin JSON API:

```bash
agg serve                              # JSON API on :7878
cd web && npm install && npm run dev   # the web tool on :5173
```

## CLI reference

Global flags, valid on every subcommand: `--dir <path>` (project root, default `.`),
`--config <file>` (default `<dir>/agg.yaml`), `--goals <file>` (default `<dir>/goals.yaml`).

| Command | What it does | Flags |
|---|---|---|
| `agg init` | Scaffold `agg.yaml`, `goals.yaml`, `AGG_RESUME.md` and a starter judge | `--force` overwrite · `--folder` scaffold into `agg/` |
| `agg doctor` | Diagnose the setup: claude on PATH, config parses, conditions valid | |
| `agg plan` | Run every judge once and print the starting scoreboard (a dry run) | |
| `agg run` | Drive the loop until `stop_when` is met or `halt_when` fires | `--max-sessions <n>` (0 = unlimited) · `--detach` / `-d` |
| `agg judge <id>` | Run **one** goal's judge and print its raw verdict — for authoring judges | |
| `agg status` | The running loop's latest scoreboard, from its snapshot (cheap; re-runs no judges) | `--json` |
| `agg history` | This project's run history, newest first, plus lifetime totals | `--json` |
| `agg dashboard` | Live TUI. `Tab` focus · `↑↓` `PgUp` `PgDn` `g` `G` scroll · `f` follow · `q` quit | `--once` one-shot text snapshot |
| `agg serve` | JSON API for the web UI: `/api/state`, `/api/history`, `/api/health`, `POST /api/send` | `--port <n>` (7878) · `--cors-origin <url>` · `--token <t>` |
| `agg spawn` | Launch a long task that outlives the session, tracked so the reaper spares it | `--name <n>` · `--reason <why>` · `-- <cmd…>` |
| `agg stop [reason]` | Graceful stop at the next session boundary | |
| `agg inject <text>` | Prepend a high-priority instruction to the next session | |
| `agg pause` · `agg resume` | Hold the loop before the next session · continue a paused one | |
| `agg budget [total]` | Change the token ceiling (omit the value for unlimited) | |
| `agg send <cmd>` | The same steering, explicit: `inject`, `budget`, `pause`, `resume`, `stop`, `note` | |

`agg run` exit codes, so automation can branch on the outcome: **0** goals met (or an operator
stop) · **1** hard error · **3** a guard fired (`halt_when`) · **4** hit `--max-sessions`.

## Configuration

`agg.yaml` — the knobs most runs touch. **Full reference: [`docs/CONFIG.md`](docs/CONFIG.md)**

```yaml
project: my-project
model: claude-sonnet-5
resume_prompt: AGG_RESUME.md

budget: { total: 5000000 }             # output-token ceiling → over_budget
cost:   { total: 5.0 }                 # $ ceiling (API-equivalent) → over_cost
summary: { enabled: true }             # rolling LLM summary carried between sessions
memory:  { enabled: true }             # what past sessions learned, INJECTed into the next
session_isolation: { enabled: true }   # each session on its own git branch; merged only if VERIFY passes
hooks: { on_session_end: ["./scripts/refresh-index.sh"] }
```

`goals.yaml` — typed goals plus the stop and halt conditions:

```yaml
goals:
  - id: tests_pass
    type: cardinal              # binary | percentage | cardinal
    target: 28
    judge: { kind: script, cmd: "./judges/tests.sh" }
  - id: no_sorry
    type: binary
    invariant: true             # must STAY true; a regression can halt the loop
    judge: { kind: script, cmd: "./judges/no_sorry.sh" }

stop_when: tests_pass
halt_when: any_regressed(invariants) OR over_cost
```

## Examples

- **[`examples/hello-agg/`](examples/hello-agg/)** — the four-file loop above, runnable, plus a
  walkthrough that drives a project to "all tests pass".
- **[`examples/p-vs-np/`](examples/p-vs-np/)** — the showcase: every feature aimed at one famous
  problem, with a Lean proof checker as the unfakeable judge.

## How it compares

`agg` didn't invent the loop — it's one of several [Ralph-loop](https://ghuntley.com/ralph/)
implementations, and it stands on Huntley's pattern and the tools before it. What it adds is
**making the outer loop deterministic and the `VERIFY` gate unfakeable**: typed goals, regression
detection, invariant halt-on-cheating, a safe stop-condition DSL, a shipped LLM judge, a two-signal
watchdog, rate-limit backoff, process reaping, per-session git isolation with a rollback gate, and
mobile + web steering.

The feature-by-feature matrix, honest gaps and sources: **[COMPARISON.md](COMPARISON.md)**.

## License

MIT
