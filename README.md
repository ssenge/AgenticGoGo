<h1 align="center">
  <img src="assets/logo.png" width="170" alt="AgenticGoGo — a pole-dancing robot that keeps your agent going going"><br>
  AgenticGoGo
</h1>

<p align="center">
  <em>A deterministic outer&nbsp;</em>
  <a href="https://ghuntley.com/ralph/"><img src="assets/ralph-loop.svg" height="20" alt="Ralph loop" valign="middle"></a>
  <em>&nbsp;with incorruptible judges around a stochastic agent.</em>
</p>

<p align="center"><em>Stop typing “go go”.</em></p>

---

Are you constantly typing **“go go”**, **“continue”**, **“keep going”** to nudge your coding agent
through a long plan? Do even spec-driven approaches stall mid-flight, run out of context, or
quietly stop one step short — leaving you to babysit a terminal?

**Then AgenticGoGo is for you.**

AgenticGoGo (`agg`) is a deterministic outer **[Ralph loop](https://ghuntley.com/ralph/)** that
drives a stochastic inner agent — relaunching a **fresh** session, verifying its work against gates
*it can't fake*, and repeating until your goals are actually met. The loop is plain code: it never
hallucinates a decision. The agent does the work, inside one step, and never decides when it's done.

> **Claude Code only.** `agg` drives `claude -p` as its inner agent. No other coding agent is
> supported today.

<p align="center">
  <img src="assets/loop.png" alt="The four stages of the agg loop — INJECT, RUN, VERIFY, GATE — arranged in a circle" width="620">
</p>

| Stage | What it does | Who runs it |
|---|---|---|
| **`INJECT`** | Builds the agent's prompt: your standing instruction, what past sessions learned, any steering you queued. | code |
| **`RUN`** | Launches one **fresh** Claude Code session (`claude -p`). It edits files. It never decides whether it succeeded. | **the agent** |
| **`VERIFY`** | `agg` runs your **judges** itself, against the filesystem. The agent is never asked to grade its own homework. | code |
| **`GATE`** | Keeps or rolls back the work, checks `stop_when` / `halt_when`, carries state forward — or stops. | code |

Three of the four stages are plain Rust. Only `RUN` is stochastic. That is why `VERIFY` is the
moat: a gate the agent cannot corrupt, because the agent never runs it.

## Quick start

Say you have a project with a broken `calc.py` and you want the tests green.

**1 — Let Claude set the loop up.** In a Claude Code session, in your project:

```
/agg:new
```

`/agg:new` reads whatever planning material you already have — a PRD, a ROADMAP, `.planning/`, a
README, or just the code — asks you about anything it can't infer, shows you what it intends to
write, and then writes the loop's config into `agg/`:

```yaml
# agg/goals.yaml — what "done" means, and who decides
goals:
  - id: tests_pass
    type: binary
    judge: { kind: script, cmd: "./agg/judges/tests.sh" }
stop_when: tests_pass
```

```yaml
# agg/agg.yaml — how the loop runs
project: calc
model: claude-sonnet-5
resume_prompt: AGG_RESUME.md
```

`AGG_RESUME.md` is the standing instruction `INJECT`ed into *every* session — "make the tests in
`tests/` pass; don't weaken the tests."

**2 — Have Claude write the judge.** A judge is any command that prints one line of JSON. Ask for it:

> Write `agg/judges/tests.sh`: run `pytest -q`, and print `{"met":true}` if it exits 0,
> otherwise `{"met":false,"rationale":"<the first failing test>"}`. Nothing else on stdout.

```bash
#!/usr/bin/env bash
# agg/judges/tests.sh — VERIFY. agg runs this; the agent never does.
if out=$(pytest -q 2>&1); then
  echo '{"met":true}'
else
  fail=$(printf '%s' "$out" | grep -m1 FAILED || echo "tests failed")
  printf '{"met":false,"rationale":"%s"}\n' "$fail"
fi
```

**3 — Run it, and watch.**

```bash
agg plan                # dry run: one VERIFY pass, prints the scoreboard. No agent launched.
agg run --detach        # drive the loop until stop_when is met; logs to .agg/run.log
agg dashboard           # live TUI  (or: agg serve + the web UI — see Interfaces)
```

`VERIFY` rejects the failing tests → the agent edits `calc.py` → `VERIFY` re-runs `pytest` →
`GATE` sees `stop_when` met → the loop stops. Everything else is more goals, smarter judges, and
guardrails on the same four stages.

**4 — Supervise it from your phone.** Open a *second* Claude Code session and run `/agg:supervise`.
It attaches to the running loop, and you can reach that session from the Claude Code mobile app:
ask "how's it going?", and inject a course-correction when something needs your attention.

```
/agg:supervise
> how's it going?
> inject: the auth refactor is the blocker — do that first
```

The supervisor reads only `.agg/state.json` — the small scoreboard snapshot — and `agg status`. It
**never** tails the workers' output, so supervising a long run costs you almost nothing.

> No planning material and no Claude session handy? `agg init` scaffolds the same files from
> templates, without reading your project. It's the fallback, not the happy path.

## Install

`agg` drives the [Claude Code](https://claude.com/claude-code) CLI, which must be on your PATH.
Your other Claude Code plugins and MCP servers keep working inside `RUN`.

```bash
# A) one-liner (detects OS/arch; installs to /usr/local/bin, or ~/.local/bin if that's read-only)
curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/install.sh | sh

# B) prebuilt binary — see Releases (Windows: take the .exe)
# C) from source (needs the Rust toolchain)
cargo build --release && sudo cp target/release/agg /usr/local/bin/
```

Pin a version with `AGG_VERSION=v0.0.11`; choose the directory with `AGG_INSTALL_DIR=~/bin`.

The `/agg:*` skills are a **separate** install — a plugin can't put a binary on your shell PATH.
Inside Claude Code:

```
/plugin marketplace add ssenge/AgenticGoGo
/plugin install agg@agenticgogo
```

…or non-interactively, from a terminal:

```bash
claude plugin marketplace add ssenge/AgenticGoGo
claude plugin install agg@agenticgogo --scope user
```

## Steering a running loop

You can't interrupt a headless agent mid-thought (a platform limit), so steering is
**session-granular**: queued on a bus, applied at the next `INJECT`.

```bash
agg send inject "focus on the auth module; it's the blocker"
agg send budget 8000000        # change the token ceiling mid-run
agg send pause                 # …and `agg send resume`
agg stop "done for today"      # graceful stop at the next GATE
```

`Ctrl-C` or `agg stop` shuts the agent and the loop down cleanly — no orphaned worker, ledger
finalized, base branch untouched.

## Building judges

A judge is *any command that prints this JSON to stdout*. That's the whole contract:

```json
{"met": false, "value": 18, "max": 28, "target": 28, "rationale": "18/28 tests pass"}
```

`value`/`max`/`target` are optional — a `binary` goal needs only `met`. `script` judges run a
command. `llm` judges run a `claude -p` call against a rubric, in a fresh session that loads only
**your** settings, never the agent-mutated repo's `.claude/` config, so the agent can't steer its
own judge. Ready-made ones live in [`plugin/judges/`](plugin/judges/) (`cargo_test`, `cmd_exit`,
`grep_count`) and [`plugin/rubrics/`](plugin/rubrics/).

The rule that makes it a moat: **`agg` runs the judge, never the agent.** So make the judge check
the artifact — tests, a compiler, a proof checker — not the agent's claim about it.

## Interfaces

| | |
|---|---|
| **TUI** | `agg dashboard` — live scoreboard, goals, activity tail. `Tab` focus · `↑↓` `PgUp` `PgDn` `g` `G` scroll · `f` follow · `q` quit. `--once` prints a one-shot snapshot for CI/SSH. |
| **Web** | A standalone SvelteKit app in [`web/`](web/). The binary stays UI-free and exposes a thin JSON API. |
| **Supervisor** | `/agg:supervise` in a second Claude Code session — status and steering by chat, reachable from the mobile app. Reads the snapshot only, not the workers' output. |

```bash
agg serve                              # JSON API on :7878
cd web && npm install && npm run dev   # the web tool on :5173
```

## CLI reference

Global flags, valid on every subcommand: `--dir <path>` (project root, default `.`),
`--config <file>` (default `<dir>/agg.yaml`), `--goals <file>` (default `<dir>/goals.yaml`).

| Command | What it does | Flags |
|---|---|---|
| `agg init` | Scaffold starter config from templates (the fallback for `/agg:new`) | `--force` overwrite · `--folder` scaffold into `agg/` |
| `agg doctor` | Diagnose the setup: claude on PATH, config parses, conditions valid | |
| `agg plan` | Run every judge once and print the starting scoreboard (a dry run) | |
| `agg run` | Drive the loop until `stop_when` is met or `halt_when` fires | `--max-sessions <n>` (0 = unlimited) · `--detach` / `-d` |
| `agg judge <id>` | Run **one** goal's judge and print its raw verdict — for authoring judges | |
| `agg status` | The loop's latest scoreboard, from its snapshot (cheap; re-runs no judges) | `--json` |
| `agg history` | This project's run history, newest first, plus lifetime totals | `--json` |
| `agg dashboard` | Live TUI | `--once` one-shot text snapshot |
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

You don't normally write this by hand — `/agg:new` generates `agg/agg.yaml` and `agg/goals.yaml`
for your project, and `agg run` auto-detects the `agg/` folder. When you want to tune it: token and
dollar ceilings, the rolling summary, cross-session memory, per-session git isolation with a
rollback gate, lifecycle hooks, watchdog thresholds, goal types and the stop/halt condition DSL are
all documented in **[`docs/CONFIG.md`](docs/CONFIG.md)**.

## Examples

- **[`examples/hello-agg/`](examples/hello-agg/)** — the smallest possible loop, runnable in a
  minute, plus a walkthrough that drives a project to "all tests pass".
- **[`examples/p-vs-np/`](examples/p-vs-np/)** — the showcase: every feature aimed at one famous
  problem, with a Lean proof checker as the incorruptible judge.

## License

MIT
