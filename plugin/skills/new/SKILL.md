---
description: Set up AgenticGoGo for the current project — read existing plans, then generate goals.yaml, agg.yaml, and AGG_RESUME.md so `agg run` can drive the work to completion. Use when the user wants to turn a plan/spec/roadmap into an autonomous agent loop.
disable-model-invocation: false
---

# /agg:new — set up an AgenticGoGo loop for this project

You are setting up **AgenticGoGo** (`agg`): a loop that runs fresh headless agent workers
until **goal-based stop conditions** are met. Your job in this skill is to turn
whatever planning material already exists into three files the loop reads:

- `goals.yaml` — the goals, their judges, and the stop condition
- `agg.yaml` — loop config (agent, model, heartbeat, watchdog, budget, summaries)
- `AGG_RESUME.md` — the "fat" resume prompt fed to every worker session

**Core principle: do NOT replicate spec tooling.** Read what's already there and *translate*
it into goals. Only ask the user for what's genuinely missing.

`agg` can drive **Claude Code, OpenAI Codex, or GitHub Copilot** — chosen by the `agent:` key you
write into `agg.yaml`. They are **not interchangeable**, and `agg` REFUSES to start a run whose
config asks for something the chosen agent cannot do. **Step 0 is therefore not optional: get the
agent right, or the config you generate will not start.**

---

## Step 0 — Pick the agent the loop will drive (do this FIRST)

Which agent runs the *inner workers*. This is independent of whichever agent is running THIS
skill — but "the one I'm already using" is the sane default, so offer it first.

See which are actually installed, and ask the user (default: the agent you are running in):

```bash
claude --version; codex --version; copilot --version
```

Write the answer as `agent: claude|codex|copilot` in `agg.yaml`. Then obey the matrix below.

### The capability matrix — VERIFIED, do not re-derive

| | claude | codex | copilot |
|---|---|---|---|
| script judges | ✅ | ✅ | ✅ |
| LLM judges (`judge: {kind: llm}`) | ✅ | ✅ | ✅ |
| summaries (`summary.enabled`) | ✅ | ✅ | ✅ |
| token budget (`budget.total`, `over_budget`) | ✅ | ✅ | ✅ |
| session resume (`resume_sessions`) | ✅ | ✅ | ✅ |
| thinking effort (`effort:`) | ✅ | ✅ (clamps `max`→`high`) | ✅ |
| rate-limit backoff | ✅ | ✅ | ❌ |
| **dollar cost (`cost.total`, `over_cost`)** | ✅ | ❌ | ❌ |

### Four hard rules. Break one and the config you write CANNOT START.

1. **`cost.total` and `halt_when: over_cost` are CLAUDE-ONLY.** Codex and Copilot cannot price a
   session in dollars, so `agg` refuses the config outright rather than let a spend guard silently
   never fire. For codex/copilot use **`budget.total` (output tokens)** instead — it works on all
   three. (Copilot can additionally cap itself with `--max-ai-credits <n>` via `worker_args`.)
2. **Codex: OMIT `model:` entirely** unless the user explicitly names one. Guessing (e.g.
   `gpt-5-codex`) is a hard 400 — *"not supported when using Codex with a ChatGPT account"*. Which
   models exist depends on how the user authenticated, so let `agg` pick its default.
3. **Copilot: `model: auto` is safe.** Claude: `model: "claude-opus-4-8[1m]"` is the default.
4. **Finish by running `agg doctor`** (Step 7). It re-checks every rule above against the real
   backend. It is a free correctness check — if it passes, the config starts.

## Step 1 — Discover existing plans (read, don't ask yet)

Look for planning artifacts, in this priority order. Read whatever exists:

1. `.planning/` (get-shit-done: PROJECT.md, ROADMAP.md, phase SPEC/PLAN files)
2. `PRD.md`, `SPEC.md`, `ROADMAP.md`, `REQUIREMENTS.md`, `DESIGN.md`
3. `README.md`
4. Recent `git log` (last ~30 commits) for the project's trajectory
5. If a knowledge graph exists (`graphify-out/`), use it to understand structure

If **engram** is available, run `mem_search` for the project to recover prior context.

If you find NOTHING actionable (empty/new repo), go to Step 5 and ask the user to describe
the goal in a few targeted questions — but prefer inference whenever material exists.

## Step 2 — Derive goals

From the plans, derive a small set of **concrete, checkable goals** (aim for 3–8). For each:

- **id**: short snake_case (e.g. `tests_pass`, `modules_migrated`, `api_documented`)
- **type**: one of
  - `binary` — done yes/no (e.g. "all tests pass")
  - `percentage` — a 0–100 measure vs a target (e.g. "≥90% coverage")
  - `cardinal` — N of M (e.g. "18 of 28 problems solved")
- **target**: the threshold to be "met"
- **description**: one line, human-readable

Mark any **soundness/invariant** goal with `invariant: true` (things that must STAY true —
"never break the build", "no wrong results"). These can guard the loop via `halt_when`.

**Set a `recheck` policy to avoid re-judging finished goals** (saves tokens, esp. with LLM
judges). Default is `always` (re-judge every cycle — REQUIRED for invariants). For a goal
whose status can't change once achieved (a written doc, a completed study), use
`recheck: once_met` — it latches after first met and its judge never runs again. For a goal
gated on a specific artifact, use `recheck: on_change` with `recheck_inputs: [files]` — it
re-judges only when those files change. (agg rejects `once_met` on an invariant.)

## Step 3 — Pick a judge per goal

Every goal needs a **judge** that emits a verdict JSON:
`{"met": <bool>, "value": <num>, "max": <num>, "target": <num>, "rationale": "<one line>"}`

Two kinds:

- **`script`** (preferred when measurable) — a command whose stdout is the verdict JSON.
  Suggest a real command for THIS project (a test runner, a benchmark, a coverage tool, a
  grep-count). If the project has such a command, write a tiny wrapper script under
  `judges/` that runs it and prints the verdict JSON. Example:
  ```yaml
  judge:
    kind: script
    cmd: "./judges/tests.sh"
    timeout: 300
  ```
- **`llm`** (for qualitative goals) — a tools-off one-shot model call with a **rubric** that scores
  artifacts. Works on **all three agents**. Generate a rubric file under `rubrics/<id>.md` with
  explicit criteria ending in the required line:
  *"Output ONLY the verdict JSON: {met, value, max, target, rationale}."*
  ```yaml
  judge:
    kind: llm
    model: haiku          # claude: haiku · copilot: auto · codex: OMIT this line
    rubric: "rubrics/<id>.md"
    inputs: ["diff", "log:logs/test.out", "src/main.rs"]
    timeout: 120
  ```
  The `model:` here follows **the same rule as Step 0**: a cheap model on Claude (`haiku`), `auto`
  on Copilot, and **omitted entirely on Codex** (naming one is a hard 400).
  Valid `inputs` tokens: `"diff"`, `"diff:<rev>"`, `"status"`, `"log:<path>"` (tail), or a file path.

## Step 4 — Choose the stop condition

`stop_when` is a whitelisted expression over goals (NOT arbitrary code). Available terms:
goal ids (→ their met bool), `all_goals`, `count_met`, `total`, `met_fraction`,
`weighted_fraction`, `any_regressed(invariants)`, `wall_hours`, and three **ceiling guards**:
- `over_budget` — output **tokens** exceed `budget.total` (agg.yaml). **Works on all three agents.**
- `over_cost` — **dollars** exceed `cost.total` (agg.yaml). **CLAUDE ONLY** — only Claude reports a
  price. Emitting this for codex/copilot makes the config refuse to start (Step 0, rule 1).
- `over_iterations` — **sessions** reach the `--max-sessions` cap

- Default: `stop_when: "all_goals"`
- Statistical: `stop_when: "met_fraction >= 0.75"` or `"count_met >= 3"`
- Boolean: `stop_when: "goal_a OR goal_b"`

Add a **`halt_when`** guard if there are invariants or you want a ceiling brake. The ceilings
OR together — the loop halts the moment ANY one trips:

```yaml
# claude:
halt_when: "any_regressed(invariants) OR over_cost OR over_budget OR over_iterations OR wall_hours >= 8"
# codex / copilot — SAME, minus over_cost (they cannot report dollars):
halt_when: "any_regressed(invariants) OR over_budget OR over_iterations OR wall_hours >= 8"
```

**Never leave an autonomous loop with no ceiling at all.** If you drop `over_cost` for
codex/copilot, you MUST keep `over_budget` (with a real `budget.total`) in its place.

## Step 4.5 — Detect the user's tools and offer to wire them in (NO hardcoded tool list)

agg the binary is tool-agnostic — it only runs generic lifecycle hooks. But the worker
runs in THIS user's environment and inherits whatever tools the session has. A worker that
USES those tools (a code graph instead of grepping, a memory tool to recall state across
sessions) is cheaper and smarter. So: **enumerate the tools that are actually active in this
session, then ASK the user which to wire into the loop.** Do NOT assume any specific tool
exists — discover them.

**Enumerate for the agent the loop will DRIVE (Step 0) — not for whichever agent runs this skill.
Report only what's actually present:**

1. **MCP servers** — the worker inherits the live ones; their tools appear as `mcp__<server>__<tool>`.
   - claude: `claude mcp list` (each `✓ Connected` line)
   - codex: `codex mcp list`
   - copilot: `copilot mcp list`
2. **Skills** — each is a capability the worker inherits. Look where THAT agent looks:
   - claude: `~/.claude/skills/` + plugin skill dirs
   - codex: `.agents/skills/`, `~/.agents/skills/`, `~/.codex/skills/`
   - copilot: `copilot skill list` (lists every source at once — and costs no quota)
3. **Hooks** — a global settings hook (e.g. a command-rewrite proxy) is inherited by the headless
   worker automatically. It needs NO agg wiring — just note it.
   - claude: a `"hooks"` block in `~/.claude/settings.json`

**Then, for each tool that plausibly helps a long autonomous loop, ASK the user (ONE question)
whether to wire it in — and infer HOW from the tool's own purpose:**
- A **code-graph / indexer** tool → offer: `hooks.on_start` to build it, `hooks.on_session_end`
  (or `background`) to keep it fresh, and a `prompt_includes` line telling the worker to query
  it instead of grepping. (Refresh matters: the graph must track code changes between sessions.)
- A **memory tool** (persistent across sessions) → offer a `prompt_includes` line telling the
  worker to recall state at session start and save a handoff note at session end (cheaper than
  re-deriving every fresh session).
- A **token/cost proxy hook** already in global settings → just inform the user it's inherited
  automatically; nothing to configure.
- **Anything else** (a linter, a test-cache warmer, a custom CLI) → ask if they want a hook,
  and let them name the command. The mechanism is identical regardless of the tool.

**Rules:** never invent a tool that isn't present; only offer what you actually detected.
Phrase each offer concretely ("Wire `<tool>`? I'd add `on_start: [<cmd>]` and a prompt note
to use it"). Only write hooks the user confirms. If the user declines all, write no hooks —
that's fine. The exact hook command depends on the tool's own CLI; read its `--help` or skill
doc if unsure, and don't guess a flag — ask the user for the command if you can't determine it.

The result goes into `agg.yaml` (`hooks:` + `prompt_includes:`) and, for prompt guidance, a
small `AGG_TOOLING.md` you reference from `prompt_includes`.

## Step 5 — Ask ONLY what's missing

Ask (with a structured picker if your agent has one, else plain questions) ONLY for genuine gaps
you couldn't infer, e.g.:
- which agent the loop should drive, if Step 0 didn't settle it
- the test/benchmark command if you couldn't find it
- the target threshold for a percentage/cardinal goal
- the token budget and max wall-time, if the user wants guards
- the inner-worker model — but see Step 0 rules 2–3: **never guess one for codex**

Show the user the proposed `goals.yaml` and let them approve or edit before writing.

## Step 6 — Write the three files

Write into an **`agg/` config folder** by DEFAULT — it keeps the project root clean and is what
the README documents. Fall back to the project root only if the user explicitly asks for it. `agg run` auto-detects either: if `<project>/agg/`
exists, it reads `agg/agg.yaml`, `agg/goals.yaml`, the resume prompt, and `agg/judges/`,
`agg/rubrics/` from there; otherwise it reads them from the root. Prefer the folder when you're
generating several judges and/or rubrics (it stops them cluttering the project root); keep the
root for a tiny 1-judge setup. Two rules if you use the folder:
- **resume prompt + rubric files resolve against `agg/`** (put them inside it, reference them
  by name as today, e.g. `rubric: "rubrics/<id>.md"` → `agg/rubrics/<id>.md`).
- **judge `cmd` + `inputs` resolve against the PROJECT ROOT** (scripts run there). So a foldered
  judge is `cmd: "./agg/judges/<id>.sh"` — root-relative, with the `agg/` prefix.
You can also scaffold the folder layout directly with `agg init --folder`.

### `goals.yaml`
```yaml
goals:
  - id: <id>
    type: <binary|percentage|cardinal>
    target: <n>
    description: "<one line>"
    judge: { kind: script, cmd: "./judges/<id>.sh", timeout: 300 }
    # or invariant: true for guards
stop_when: "<expression>"
halt_when: "<expression>"   # optional
```

### `agg.yaml` — **the agent-specific file. Get this wrong and the loop won't start.**

Common to every agent:
```yaml
project: <name>
agent: <claude|codex|copilot>          # from Step 0 — REQUIRED
resume_prompt: "AGG_RESUME.md"
heartbeat_secs: 30
watchdog: { idle_secs: 900, cpu_grace: 180 }
budget: { total: <tokens or null> }   # token ceiling → over_budget. Works on ALL agents.
summary: { enabled: true, min_interval_secs: 300 }
# hooks + prompt_includes: ONLY if Step 4.5 wired tools the user confirmed. Omit otherwise.
# hooks:
#   on_start:       ["<build-graph-cmd>"]      # whatever the detected tool needs
#   on_session_end: ["<refresh-cmd>"]
#   background:     ["<watch-cmd>"]            # reaped automatically on stop
# prompt_includes: ["AGG_TOOLING.md"]
```

Then add ONLY the lines your agent supports:

```yaml
# ── agent: claude ────────────────────────────────────────────────
model: "claude-opus-4-8[1m]"
cost: { total: <dollars or null> }    # dollar ceiling → over_cost.  CLAUDE ONLY.
summary: { enabled: true, model: haiku, min_interval_secs: 300 }
ratelimit_backoff_secs: 1800

# ── agent: codex ─────────────────────────────────────────────────
# NO `model:` line at all — naming one is a hard 400 on a ChatGPT account.
# NO `cost:` line — codex reports no dollars; agg would refuse the config.
ratelimit_backoff_secs: 1800

# ── agent: copilot ───────────────────────────────────────────────
model: auto
summary: { enabled: true, model: auto, min_interval_secs: 300 }
# NO `cost:` line — copilot bills in AI Credits, not dollars; agg would refuse the config.
# NO `ratelimit_backoff_secs` — copilot cannot flag a rate-limit.
# Optional self-cap, since agg's dollar guard is unavailable:
# worker_args: ["--max-ai-credits", "50"]
```

### `AGG_RESUME.md` (the fat resume prompt — this is the worker's standing instructions)
Write a self-contained prompt that, on EVERY fresh session, tells the worker to:
1. Read its handoff/state (a `HANDOFF.md` you also create, or the project's existing one)
2. Do ONE self-contained chunk of work toward the goals
3. Commit as it goes
4. Before exiting (context fills — a headless worker does NOT auto-compact): rewrite the handoff
   with new state + the exact next task, commit
5. Be autonomous — there is NO human in the loop; never pause to ask

Inline any workflow content the worker needs — do not assume it can invoke a skill by name in a
headless session. Paste the relevant steps directly.

Also create a starter `HANDOFF.md` capturing the current state + first task.

## Step 7 — Validate: `agg doctor`, then `agg plan`

**`agg doctor` first — it is the check that catches a config the chosen agent cannot honour**
(a cost guard on codex, a model name codex will reject, a stray `effort:`). It verifies the agent
CLI is installed AND that every key you wrote is one that agent can actually deliver:
```bash
agg doctor
```
Fix anything it marks ✗ — a ✗ here means `agg run` would refuse to start.

Then the dry-run, to confirm the judges work and show the starting scoreboard:
```bash
agg plan
```
If a judge errors, fix its command/rubric before finishing.

## Step 8 — Tell the user how to launch

```
Setup complete — driving <agent>. Starting scoreboard above.

To run the loop:
  agg run                # foreground, watch it live
  agg run --detach       # background (pidfile + .agg/run.log), survives the terminal

To watch the dashboard (second terminal):
  agg dashboard

To stop / steer:
  agg stop               # graceful stop at the next session boundary
  agg send inject "…"    # high-priority instruction for the next session

The loop stops when:  <stop_when>
```

(If `agg` is not on PATH, tell them to install it — the install script or GitHub Releases, see
the repo README — since these skills ship without the CLI binary.)
