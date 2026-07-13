<h1 align="center">
  <img src="assets/logo.png" width="170" alt="AgenticGoGo — a pole-dancing robot that keeps your agent going going"><br>
  AgenticGoGo
</h1>

<p align="center">
  <em>A deterministic outer Ralph loop with incorruptible judges around a stochastic inner agent.</em>
</p>

<p align="center"><em>Stop typing “go go”.</em></p>

---

Are you constantly typing **“go go”**, **“continue”**, **“keep going”** to nudge your coding agent
through a long plan? Do even spec-driven approaches stall mid-flight, run out of context, or
quietly stop one step short — leaving you to babysit a terminal?

**Then AgenticGoGo is for you.**

AgenticGoGo (`agg`) is a deterministic outer **[Ralph loop](https://ghuntley.com/ralph/)** that
drives a stochastic inner coding agent (currently supported: [Claude Code](https://claude.com/claude-code), [OpenAI Codex](https://developers.openai.com/codex/cli), [GitHub Copilot](https://github.com/github/copilot-cli)) — relaunching a **fresh** session, verifying its work against gates
*it can't fake* (the **judges**), and repeating until your goals are actually met. The loop is plain code: it never hallucinates a decision. The agent does the work, inside one step, and never decides when it's done. *(A similar LLM-based approach — generate → verify → keep, as in evolutionary code search — was
proposed years ago, outside the Ralph-loop community, by DeepMind's
[AlphaCode](https://arxiv.org/abs/2203.07814) and its open-source variant
[CodeEvolve](https://arxiv.org/abs/2510.14150).)*

A **judge** is a small, incorruptible check that decides whether one goal is met — usually a script
inspecting the artifact (tests, a compiler, a proof checker), or an LLM grading against a rubric. You
compose several with a boolean grammar (`and` / `or` / `not`, e.g. `outputs_two and tests_pass`) to
say exactly what "done" means.

<p align="center">
  <img src="assets/loop.png" alt="The four stages of the agg loop — INJECT, RUN, VERIFY, GATE — arranged in a circle" width="620">
</p>

| Stage | What it does | Who runs it |
|---|---|---|
| **`INJECT`** | Builds the agent's prompt: your standing instruction, what past sessions learned, any steering you queued. | code |
| **`RUN`** | Launches one **fresh** agent session (`claude -p` · `codex exec` · `copilot -p`). It edits files. It never decides whether it succeeded. | **the agent** |
| **`VERIFY`** | `agg` runs your **judges** itself. The agent is never asked to grade its own homework. | code |
| **`GATE`** | Keeps or rolls back the work, checks `stop_when`, carries state forward — or stops. | code |

Three of the four stages are deterministic code; only the `RUN` stage is a (stochastic) coding agent.
The loop continues until all goals are met — potentially for hours, days, weeks (watch your token
consumption 😉). Because the agent never runs `VERIFY`, it can't fake the gate that decides it's done.

**A stronger model gets there faster; the judge is what makes "there" mean anything.** So the loop
works when "done" is something *other than the agent* can check — *"`solve(Y)` returns `X`"*, *"18 of
28 benchmarks pass"*, *"`f(x)` runs in under 200 ms"*, *"the report scores ≥ 85% against this
rubric"* — or any boolean combination of such judges. A goal like *"make the code nicer"* gives the
loop nothing to gate on, and a vague goal is a gameable one.

The overall architecture is captured in the following diagram:

<p align="center">
  <img src="assets/arch.png" alt="AgenticGoGo architecture: the agg outer loop drives one fresh agent worker (claude -p / codex exec / copilot -p) and writes plain state files under .agg/, which the TUI, the web UI, and an agent supervisor session (reachable from your phone) all read" width="760">
</p>

The whole system is **the loop plus plain files**: `agg` drives one fresh worker and writes state to
`.agg/`; the TUI, the web UI, and an `/agg:supervise` agent session (reachable from your phone, via Claude
Code's mobile app) all just *read* those files (and the supervisor can *steer* via the bus). More on that in
[State and memory](#state-and-memory) and [Interfaces](#interfaces).

## Quick start

Say you have a project with a broken `calc.py` and you want it fixed:

```python
# calc.py — BROKEN on purpose
def add(a, b):
    return a * b            # bug: should be a + b

if __name__ == "__main__":
    print(add(1, 1))        # prints 1; should print 2
```

*(A deliberately trivial example — a one-line fix where `agg` buys you nothing over just doing it
yourself. It's here to show the mechanics; the real payoff is long, multi-hour work you'd otherwise
have to babysit.)*

**0 — Install.** The binary:

```bash
curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/scripts/install.sh | sh
```

Then the `/agg:*` skills. **All three agents have a plugin marketplace, and all three take the same
one** — add the marketplace, install the plugin:

```
# Claude Code — inside a session
/plugin marketplace add ssenge/AgenticGoGo
/plugin install agg@agenticgogo
```
```bash
# OpenAI Codex
codex plugin marketplace add https://github.com/ssenge/AgenticGoGo
codex plugin add agg@agenticgogo
```
```bash
# GitHub Copilot
copilot plugin marketplace add ssenge/AgenticGoGo
copilot plugin install agg@agenticgogo
```

Or install them straight into **this project**, with the binary you just installed — no marketplace,
same three skills, every agent:

```bash
agg skills install --agent codex     # claude | codex | copilot · --user for account-wide
                                     # (agg needs to know WHICH agent — it installs into a
                                     #  different directory for each, and this runs before
                                     #  agg.yaml exists. Inside an agent's own shell it detects it.)
```

Full options (prebuilt binaries, from source, version pinning) →
**[docs/INSTALL.md](docs/INSTALL.md)**.

**1 — Prerequisite: a coding agent.** `agg` drives one headlessly. Install and authenticate whichever
you want, and check it works:

| agent | `agg.yaml` | install + authenticate | check it works headlessly |
|---|---|---|---|
| **[Claude Code](https://claude.com/claude-code)** *(default)* | `agent: claude` | `npm i -g @anthropic-ai/claude-code` · `claude auth login` | `claude -p "hello"` |
| **[OpenAI Codex](https://developers.openai.com/codex/cli)** | `agent: codex` | `npm i -g @openai/codex` · `codex login` | `codex exec "hello"` |
| **[GitHub Copilot CLI](https://github.com/github/copilot-cli)** | `agent: copilot` | `npm i -g @github/copilot` · `copilot login` | `copilot -p "hello"` |

The **check** column is the one that matters: `agg` only ever drives the agent *headlessly*, so a
version number or a login status proves nothing. If that one-shot prompt answers, `agg` can drive it.

`agg doctor` verifies the agent is on your PATH **and** that it can do what your config asks —
run it before your first loop. See [Choosing an agent](#choosing-an-agent) for what each one
can't do.

**2 — Let the agent set up the loop.** Invoke the skill: **`/agg:new`** (Claude Code), **`/agg-new`**
(Copilot), **`$agg-new`** (Codex — it uses `$`, not `/`). On any of them you can equally just *ask*
("set up AgenticGoGo for this project") and the agent picks the skill up by its description. Either
way it turns *your* definition of "done" into config. Point it at a spec you already have (a PRD,
ROADMAP, README, `.planning/`), or just say what you want — e.g.

> `/agg:new` — done = `python3 calc.py` prints `2` **and** `pytest -q` passes

It reads that plus your code, shows the `goals.yaml` it proposes, and lets you edit before writing it
into `agg/`. The result might look like:

```yaml
# agg/goals.yaml — what "done" means, and who decides
goals:
  - id: outputs_two          # behaviour: the program actually prints 2
    type: binary
    judge: { kind: script, cmd: "./agg/judges/outputs_two.sh" }
  - id: tests_pass           # regression safety: the whole suite is green (an inline judge)
    type: binary
    judge: { kind: script, cmd: "pytest -q >/dev/null 2>&1 && echo '{\"met\":true}' || echo '{\"met\":false}'" }
stop_when: outputs_two and tests_pass
```

```yaml
# agg/agg.yaml — how the loop runs
agent: claude              # claude (default) · codex · copilot — see "Choosing an agent"
project: calc
model: "claude-opus-4-8[1m]"   # claude only. On codex: OMIT this line. On copilot: `auto`.
resume_prompt: AGG_RESUME.md
```

`AGG_RESUME.md` is the standing instruction `INJECT`ed into *every* session — "fix `calc.py` so
`python3 calc.py` prints 2 and the tests pass". In case of multiple iterations (i.e. multiple subsequent **`RUN`** stages), this file gets adapted to track the progress so far accordingly (see [State and memory](#state-and-memory)).
Also note the `stop_when: outputs_two and tests_pass` line in the `goals.yaml` file: it composes two judges with an `and`, and the next step explains how to create such judges.

**3 — Have the agent write the judge.** A judge is any command that prints a verdict as JSON (see
[Building judges](#building-judges)). You can write one by hand, but asking your agent is the easy way:

> **Prompt:** Write the judge `agg/judges/outputs_two.sh`: run `python3 calc.py`, print `{"met":true}` if its
> output is exactly `2`, else `{"met":false,"rationale":"calc.py did not print 2"}`. Nothing else on
> stdout.

The result might look like:

```bash
#!/usr/bin/env bash
# agg/judges/outputs_two.sh — VERIFY. agg runs this; the agent never does.
[ "$(python3 calc.py 2>/dev/null)" = "2" ] \
  && echo '{"met":true}' \
  || echo '{"met":false,"rationale":"calc.py did not print 2"}'
```

The second goal, `tests_pass`, needs no script at all — an inline shell command that prints the verdict is a perfectly good judge (`pytest -q` must exit 0).
The `stop_when: outputs_two and tests_pass` line above requires **both**. Why chain them? Each catches
what the other misses: `outputs_two` alone is gameable — the agent could just hardcode `print(2)` —
but `tests_pass`, which checks `add(2, 3) == 5`, would still be red. That `and` is judge chaining:
any boolean of goal ids (`and` / `or` / `not`, with parentheses) is a valid `stop_when`.

**4 — Run it, and watch.**

```bash
agg plan                # dry run: one VERIFY pass, prints the scoreboard. No agent launched.
agg run --detach        # drive the loop until stop_when is met; logs to .agg/run.log
agg dashboard           # live TUI  (or: agg serve + the web UI — see Interfaces)
```

`VERIFY` rejects the broken `calc.py` → the agent fixes `a * b` → `a + b` → `VERIFY` re-runs both
judges → `GATE` sees `stop_when` met → the loop stops. This toy finishes in a **single iteration**,
and there's no knob to force more — the loop stops the moment `stop_when` is true. Real projects run
as many iterations as it takes to satisfy every goal, sometimes hundreds, each a fresh session.

**5 — Optionally, supervise from a second agent session.** Open a second session **in the same project
folder** and invoke the supervisor skill — `/agg:supervise` (Claude), `/agg-supervise` (Copilot),
`$agg-supervise` (Codex). It's not required — a plain session could read the state and run `agg send`
too — but the skill hands that session the right playbook: read the compact scoreboard (never the
worker firehose, which would blow up your token bill), the steering vocabulary, and what to watch for.
And because a Claude Code session is reachable from the mobile app, you can check in and
course-correct from your phone:

```
/agg:supervise
> how's it going?
> inject: the auth refactor is the blocker — do that first
```

The supervisor reads only `.agg/state.json` — the small scoreboard snapshot — and `agg status`. It
**never** tails the workers' output, so supervising a long run costs you almost nothing.

## Choosing an agent

```yaml
# agg.yaml
agent: claude     # claude (default) · codex · copilot
```
Supported features:

| | Claude | Codex | Copilot |
|---|---|---|---|
| Runs the loop, edits files | ✅ | ✅ | ✅ |
| Script judges | ✅ | ✅ | ✅ |
| **LLM judges** (`judge: { kind: llm }`) | ✅ | ✅ | ✅ |
| **Progress summaries** (`summary.enabled`) | ✅ | ✅ | ✅ |
| Token budget (`budget.total`, `over_budget`) | ✅ | ✅ | ✅ |
| Session resume (`resume_sessions`) | ✅ | ✅ | ✅ |
| Thinking effort (`effort:`) | ✅ | ✅ | ⚠️ ¹ |
| Rate-limit backoff | ✅ | ✅ | ❌ |
| **Dollar cost cap** (`cost.total`, `over_cost`) | ✅ | ❌ | ❌ |

- **`model:`** — **Codex: omit it entirely.** Which models you may use depends on how you
  authenticated, and naming a wrong one is a hard 400 at runtime. **Copilot: `model: auto`.**
- **`effort:`** takes `low|medium|high|xhigh|max`. Codex tops out at `high` (`max` clamps to it).
  ¹ **Copilot rejects `effort:` together with `model: auto`** — which is its default. Name a concrete
  model, or leave `effort:` empty. `agg` refuses the pair at startup rather than letting every
  session die.
- **Dollar cost** is Claude-only: Codex reports no cost at all, Copilot bills in AI Credits, not
  dollars. Ask for `cost.total` / `over_cost` on either and `agg run` **refuses to start** — a spend
  guard that can never fire is worse than none. Use `budget.total` (tokens), which works everywhere.
  Copilot can also cap itself: `worker_args: ["--max-ai-credits", "50"]`.

### Setting up on Codex or Copilot

The `/agg:*` skills work on **all three agents**, by two routes.

**Route 1 — the plugin marketplace** (see [Quick start](#quick-start)). Codex and Copilot both have one, and
both consume the *same* manifest Claude does, so there is one plugin, not three.

**Route 2 — install into the project**, with the binary you already have:

```bash
agg skills install --agent codex   # claude | codex | copilot
agg skills install --user          # …for your whole account instead of just this project

# --agent is optional once agg.yaml exists (it reads the `agent:` key), and inside an agent's own
# shell agg detects which one you are in. Otherwise name it — the install directory differs per
# agent, so a wrong guess puts the skills where that agent will never look.
```

That copies the three skills — `agg-new`, `agg-status`, `agg-supervise` — into the directory your
agent actually reads. (Note the namespace differs by route: the `agg:` in `/agg:new` comes from the
plugin, so via this installer Claude gives you `/agg-new`.) The directory differs, which is the only reason this is a command and not a `cp`:

| agent | project install | `--user` install |
|---|---|---|
| Claude Code | `.claude/skills/` | `~/.claude/skills/` |
| OpenAI Codex | `.agents/skills/` | `~/.agents/skills/` |
| GitHub Copilot | `.agents/skills/` | `~/.agents/skills/` |

`.agents/` is the emerging agent-neutral convention, and Codex and Copilot both honour it — so one
directory serves both. Claude reads neither, hence two.

**How you invoke them differs per agent** — and the prefix is not the same:

| agent | invoke it with | |
|---|---|---|
| Claude Code | `/agg:new` `/agg:status` `/agg:supervise` | `/agg-new` etc. if installed via `agg skills install` — the `agg:` namespace comes from the plugin |
| GitHub Copilot | `/agg-new` `/agg-status` `/agg-supervise` | every skill is a slash command |
| OpenAI Codex | **`$agg-new`** `$agg-status` `$agg-supervise` | Codex uses `$`, not `/` — `/agg-new` is *"Unrecognized command"*. `/skills` opens a picker. |

**Or just ask, on any of them** — every agent also selects a skill by matching your request against
its `description:`, and that is the only route that works headlessly:

```
set up AgenticGoGo for this project     → agg-new
how is the agg loop doing?              → agg-status
supervise the running agg loop          → agg-supervise
```

Then:

```bash
agg doctor     # checks the agent, that it can do what your config asks, and that the skills landed
agg run
```

`agg doctor` is the one to trust: agents are **not** interchangeable (no cost guard on Codex or
Copilot — see [Choosing an agent](#choosing-an-agent)), and `/agg:new` writes a config shaped for the agent you
chose. If doctor is green, `agg run` will start.

**No skills at all?** `agg init --agent codex` still scaffolds `agg.yaml`, `goals.yaml`,
`AGG_RESUME.md` and a starter judge — shaped for that agent (it omits the keys your agent cannot
honour, so the result starts).</br> It is the fallback, not the recommended path — `/agg:new` reads your existing
plans and derives goals and judges from them; `agg init` just writes a template.

## Features

The high-level capabilities at a glance — deeper detail lives in the linked sections and
[`docs/CONFIG.md`](docs/CONFIG.md).

**Correctness — the moat**
- **Deterministic four-stage loop** — INJECT → RUN → VERIFY → GATE; only RUN is stochastic.
- **Fresh session every iteration** — no context degradation; git + memory carry state, not a long chat.
- **Incorruptible judges** — agg runs them, the agent never grades itself ([script or LLM-as-judge](#building-judges)).
- **Boolean goal DSL** — compose judges with `and` / `or` / `not`; binary / percentage / cardinal goals + invariants.
- **Post-merge rollback gate** — a red session is reverted; the base never advances broken.

**Guardrails for unattended runs**
- **Rate-limit backoff** *(Claude + Codex)* — detects a usage/429 limit, discards the incomplete session, waits, and retries fresh.
- **Stall watchdog** — kills a worker that's gone both idle *and* CPU-flat.
- **No orphaned compute** — process-group reaping sweeps stragglers when a session or the loop ends.
- **Token ceilings on every agent, dollar ceilings on Claude** — `over_budget` / `over_iterations` / `wall_hours` everywhere; `over_cost` only where the agent reports dollars. Asking for a guard your agent can't report is refused at startup, never silently ignored.
- **Long-task tracking** — `agg spawn` keeps a sim/build alive across sessions so the reaper spares it.

**State & memory**
- **Cross-session memory** — durable `AGG_MEMORY.md`, injected into every session.
- **Plain-file state** — crash-safe and observable; no database or daemon ([details](#state-and-memory)).
- **Rolling summaries** — cheap progress digests for the dashboard and supervisor.

**Control & observability**
- **Live TUI + standalone web UI** — same state, two views ([Interfaces](#interfaces)).
- **Chat supervisor** (`/agg:supervise` · `/agg-supervise` · `$agg-supervise`) — status and steering by chat in a second agent session; from your phone via Claude Code's mobile app.
- **Session-granular steering** — `agg send inject / budget / pause / resume / stop`.
- **Automation-friendly** — `--json` output and meaningful exit codes.

**Works with your setup**
- **Claude Code, OpenAI Codex or GitHub Copilot** — subscription or API key; the worker inherits your agent's MCP servers, plugins, hooks, and skills.
- **Tool-agnostic hooks** — wire in your own code graph, linter, or memory via lifecycle hooks + prompt includes.

## Steering a running loop

You can't interrupt a headless agent mid-thought (a platform limit), so steering is
**session-granular**: queued on a bus, applied at the next `INJECT`.

```bash
agg send inject "focus on the auth module; it's the blocker"
agg send budget 8000000        # change the token ceiling mid-run
agg send pause                 # …and `agg send resume`
agg stop "done for today"      # graceful stop at the next GATE
```

Or skip the exact commands: tell your `/agg:supervise` session in plain English — *"inject: focus on
the auth module", "raise the budget to 8M", "pause for now"* — and it runs the right `agg send` for
you.

`Ctrl-C` or `agg stop` shuts the agent and the loop down cleanly — no orphaned worker, ledger
finalized, base branch untouched.

## Building judges

A judge is any command that prints a **verdict** as JSON to stdout. Only `met` is required; the rest
are optional:

```jsonc
{
  "met":       true | false,   // required — did this goal pass?
  "value":     <number>,       // optional — a count or percent (drives the progress bar)
  "max":       <number>,       // optional — the denominator for value
  "target":    <number>,       // optional — the value that counts as "done"
  "rationale": "<one line>",   // optional — shown on the dashboard
  "evidence":  ["<line>", ...] // optional
}
```

So a `binary` goal can print just `{"met": true}`; a cardinal one might print, as **one** example:

```json
{"met": false, "value": 18, "max": 28, "target": 28, "rationale": "18/28 tests pass"}
```

`agg` uses the last JSON object on stdout, so a judge can log freely and print its verdict last.

**Two flavours, same contract.** Judges are typically deterministic **scripts** or **LLM-as-judge** —
and to `agg` there's no difference: anything that emits the verdict JSON is a judge (a `script` judge
can even shell out to `claude -p` itself). The built-in **`llm`** kind is a convenience that also
*hardens* the LLM case against gaming. Give it a `rubric` + `inputs` (and optionally a `model` — **omit it to take your agent's default,
which is the only portable choice, and required on Codex**) and `agg`:

- **builds the prompt** — your rubric plus the declared input files, wrapped so the repo's content is
  fed as *untrusted data* the judge is told never to obey (a file can't talk the judge into a pass);
- **runs it isolated** — `--setting-sources user` (only *your* Claude settings, never the
  agent-mutated repo's `.claude/` config or hooks) and `--strict-mcp-config` (no MCP servers), so the
  agent can't steer the judge that grades it — and with no `--dangerously-skip-permissions`;
- **extracts the verdict** from the model's reply.

Ready-made script judges live in [`plugin/judges/`](plugin/judges/) (`cargo_test`, `cmd_exit`,
`grep_count`); rubrics in [`plugin/rubrics/`](plugin/rubrics/).

**Compose judges with Boolean grammar.** `stop_when` (and the optional `halt_when`) is any boolean of goal
ids — `a and b`, `a or b`, `not c`, with parentheses — so several judges together define "done":

```yaml
stop_when: outputs_two and tests_pass          # both must hold
halt_when: any_regressed(invariants) or over_budget   # optional guard — see docs/CONFIG.md
```

The rule that makes it a moat: **`agg` runs the judges, never the agent.** So make the judges check
the resulting artifacts — tests, a compiler, a proof checker — not the agent's claim about it — the agent will often enough hallucinate that a job is done.

## State and memory

`agg` keeps **no state in a database or a long-running daemon**. Everything is a plain file under
`<project>/.agg/` (gitignored), plus git itself. The loop is the single *writer*; every *reader* (the
TUI, the web UI, `/agg:supervise`, `agg status`) reads the same files — so you can attach any number
of views to a running loop without coupling any of them to it.

- **`.agg/state.json`** — the live scoreboard snapshot (goals, tokens, phase, activity tail), written
  atomically after each change. This is what the TUI, `agg serve`, and the supervisor read.
- **`.agg/run.pid`** · **`.agg/run.log`** — the loop's liveness (double-run guard, `agg stop` target)
  and its log when detached.
- **`.agg/bus/`** — the steering queue: `agg send …` writes a command here; the loop drains it at the
  next `INJECT`.
- **`.agg/sessions.count`** + run history (`agg history`) — counters that persist across every run.
- **`AGG_MEMORY.md`** (project root, committable) + **`.agg/memory/`** — durable cross-session memory
  the loop injects into every prompt.
- **git commits** — the actual *work* state. Each session commits; the next **fresh** session resumes
  from the filesystem + `AGG_MEMORY.md`, not from a held-open context. Git *is* the memory between
  sessions.

Because the log on stdout is the source of truth and `state.json` is just a view of it, a run is
**crash-safe and observable**: `tail`/`grep`/`cat` the files, kill and restart, inspect mid-run —
nothing important is hidden in process memory.

## Your agent's setup carries in

`agg` bundles no tools of its own. `RUN` is an ordinary `claude -p` / `codex exec` / `copilot -p`
call, so it **inherits your agent's environment** — MCP servers, plugins, settings, and hooks all keep
working inside the worker, and `/agg:new` will even detect your active MCP servers / skills / hooks
and offer to wire them into the loop (via `hooks:` + `prompt_includes:`). Nothing is hardcoded;
whatever your agent has, the worker gets.

Running an agent headlessly does impose real limits — worth knowing up front:

- **Don't rely on invoking a skill by name** inside the worker. Inline the content you need into
  `AGG_RESUME.md` rather than calling `/some-skill`.
- **No mid-session interruption** (a platform limit on all three), so steering is session-granular —
  queued on the bus, applied at the next `INJECT`.
- **The worker runs with full tool access**, because a headless agent can't answer permission
  prompts: `--dangerously-skip-permissions` (Claude), `--dangerously-bypass-approvals-and-sandbox`
  (Codex), `--allow-all-tools` (Copilot). Narrow it with `worker_args`, which passes flags **verbatim
  to your agent** — so use that agent's own vocabulary (e.g. Claude's `--allowedTools Edit,Bash`,
  Copilot's `--max-ai-credits 50`).
- **MCP servers that need interactive auth** (an OAuth login, say) may be unavailable in a headless run.
- The **judge & summarizer** sessions are deliberately **read-only**, so the agent can't steer the
  process that grades it. Same guarantee, three mechanisms: Claude `--strict-mcp-config` +
  `--setting-sources user`; Codex `--sandbox read-only`; Copilot by withholding `--allow-all-tools`.

## Interfaces

Two ways to watch a run — same live state, two views — plus a chat supervisor:

<p align="center">
  <img src="assets/dashboard.png" alt="agg dashboard — the live TUI scoreboard, goals, and activity tail" width="49%">
  <img src="assets/web.png" alt="the agg web UI — scoreboard, controls, and activity log" width="49%">
</p>
<p align="center">
  <sub><b>TUI</b> (<code>agg dashboard</code>) &nbsp;·&nbsp; <b>Web</b> (<code>agg serve</code> + the SvelteKit app in <code>src/web/</code>)</sub>
</p>

| | |
|---|---|
| **TUI** | `agg dashboard` — live scoreboard, goals, activity tail. `Tab` focus · `↑↓` `PgUp` `PgDn` `g` `G` scroll · `f` follow · `q` quit. `--once` prints a one-shot snapshot for CI/SSH. |
| **Web** | A standalone SvelteKit app in [`src/web/`](src/web/). The binary stays UI-free and exposes a thin JSON API. |
| **Supervisor** | `/agg:supervise` (Claude) · `/agg-supervise` (Copilot) · `$agg-supervise` (Codex), in a second agent session — status and steering by chat; a Claude session is reachable from the mobile app. Reads the snapshot only, not the workers' output. |

```bash
agg serve                              # JSON API on :7878
cd src/web && npm install && npm run dev   # the web UI on :5173
```

## CLI reference

Global flags, valid on every subcommand: `--dir <path>` (project root, default `.`),
`--config <file>` (default `<dir>/agg.yaml`), `--goals <file>` (default `<dir>/goals.yaml`).

| Command | What it does | Flags |
|---|---|---|
| `agg init` | Scaffold **placeholder** config you must then edit — a blank-slate fallback. Prefer `/agg:new`, which fills it in from your project. | `--force` overwrite · `--folder` scaffold into `agg/` |
| `agg doctor` | Diagnose the setup: the agent is on PATH, config parses, conditions valid, **the agent can do what the config asks**, skills installed | |
| `agg skills install` | Install the `agg-*` skills where your agent looks (`.claude/skills/` for Claude, `.agents/skills/` for Codex + Copilot) | `--agent <a>` (default: agg.yaml's `agent:`) · `--user` install under `$HOME` |
| `agg plan` | Run every judge once and print the starting scoreboard (a dry run) | |
| `agg run` | Drive the loop until `stop_when` is met (or a guard fires) | `--max-sessions <n>` (0 = unlimited) · `--detach` / `-d` |
| `agg judge <id>` | Run **one** goal's judge and print its raw verdict — for authoring judges | |
| `agg status` | The loop's latest scoreboard, from its snapshot (cheap; re-runs no judges) | `--json` |
| `agg history` | This project's run history, newest first, plus lifetime totals | `--json` |
| `agg dashboard` | Live TUI | `--once` one-shot text snapshot |
| `agg serve` | JSON API for the web UI: `/api/state`, `/api/history`, `/api/health`, `POST /api/send` | `--port <n>` (7878) · `--cors-origin <url>` · `--token <t>` |
| `agg spawn` | *(used by the worker, not to start the loop)* track a long child task so the reaper spares it and the next session polls it | `--name <n>` · `--reason <why>` · `-- <cmd…>` |
| `agg stop [reason]` | Graceful stop at the next session boundary (the one top-level steering alias) | |
| `agg send <cmd>` | All steering, applied at the next session boundary | `inject <text>` · `budget [total]` · `pause` · `resume` · `stop [reason]` · `note <text>` |

`agg run` exit codes, so automation can branch on the outcome: **0** goals met (or an operator
stop) · **1** hard error · **3** a guard fired (`halt_when`) · **4** hit `--max-sessions`.

## Configuration

You don't normally write this by hand — `/agg:new` generates `agg/agg.yaml` and `agg/goals.yaml`
for your project, and `agg run` auto-detects the `agg/` folder. When you want to tune it: the
`agent:` (see [Choosing an agent](#choosing-an-agent)), token and dollar ceilings, the rolling
summary, cross-session memory, per-session git isolation with a rollback gate, lifecycle hooks,
watchdog thresholds, goal types and the stop/halt condition DSL are all documented in
**[`docs/CONFIG.md`](docs/CONFIG.md)**.

## Examples

- **[`examples/hello-agg/`](examples/hello-agg/)** — the smallest possible loop, runnable in a
  minute, plus a walkthrough that drives a project to "all tests pass".
- **[`examples/p-vs-np/`](examples/p-vs-np/)** — the showcase: every feature aimed at one famous
  problem, with a Lean proof checker as the incorruptible judge.

## License

MIT
