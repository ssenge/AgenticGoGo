---
description: Set up AgenticGoGo for the current project — read existing plans, then generate goals.yaml, agg.yaml, and AGG_RESUME.md so `agg run` can drive the work to completion. Use when the user wants to turn a plan/spec/roadmap into an autonomous agent loop.
disable-model-invocation: false
---

# /agg:new — set up an AgenticGoGo loop for this project

You are setting up **AgenticGoGo** (`agg`): a harness that runs fresh `claude -p` workers
in a loop until **goal-based stop conditions** are met. Your job in this skill is to turn
whatever planning material already exists into three files the harness reads:

- `goals.yaml` — the goals, their judges, and the stop condition
- `agg.yaml` — harness config (model, heartbeat, watchdog, budget, summaries)
- `AGG_RESUME.md` — the "fat" resume prompt fed to every worker session

**Core principle: do NOT replicate spec tooling.** Read what's already there and *translate*
it into goals. Only ask the user for what's genuinely missing.

---

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
- **`llm`** (for qualitative goals) — a `claude -p` call with a **rubric** that scores
  artifacts. Generate a rubric file under `rubrics/<id>.md` with explicit criteria ending in
  the required line: *"Output ONLY the verdict JSON: {met, value, max, target, rationale}."*
  ```yaml
  judge:
    kind: llm
    model: haiku
    rubric: "rubrics/<id>.md"
    inputs: ["diff", "log:logs/test.out", "src/main.rs"]
    timeout: 120
  ```
  Valid `inputs` tokens: `"diff"`, `"diff:<rev>"`, `"status"`, `"log:<path>"` (tail), or a file path.

## Step 4 — Choose the stop condition

`stop_when` is a whitelisted expression over goals (NOT arbitrary code). Available terms:
goal ids (→ their met bool), `all_goals`, `count_met`, `total`, `met_fraction`,
`weighted_fraction`, `any_regressed(invariants)`, and run guards `over_budget`,
`tokens_spent`, `wall_hours`.

- Default: `stop_when: "all_goals"`
- Statistical: `stop_when: "met_fraction >= 0.75"` or `"count_met >= 3"`
- Boolean: `stop_when: "goal_a OR goal_b"`

Add a **`halt_when`** guard if there are invariants or you want a budget brake:
`halt_when: "any_regressed(invariants) OR over_budget OR wall_hours >= 8"`

## Step 4.5 — Detect the user's tools and offer to wire them in (NO hardcoded tool list)

agg the binary is tool-agnostic — it only runs generic lifecycle hooks. But the worker
runs in THIS user's environment and inherits whatever tools the session has. A worker that
USES those tools (a code graph instead of grepping, a memory tool to recall state across
sessions) is cheaper and smarter. So: **enumerate the tools that are actually active in this
session, then ASK the user which to wire into the loop.** Do NOT assume any specific tool
exists — discover them.

**Enumerate (do all three; report only what's actually present):**
1. **MCP servers** — run `claude mcp list`. Each line that shows `✓ Connected` is a live MCP
   server the worker will also inherit; its tools appear as `mcp__<server>__<tool>`.
2. **Skills** — list `~/.claude/skills/` and any plugin skill dirs; also note skills you can
   see in your own available-tools list. Each is a `/<name>` capability the worker inherits.
3. **Hooks** — check `~/.claude/settings.json` for a `"hooks"` block (e.g. a command-rewrite
   proxy). These are auto-inherited by `claude -p`; they need NO agg wiring — just note them.

**Then, for each tool that plausibly helps a long autonomous loop, ASK the user (one
`AskUserQuestion`) whether to wire it in — and infer HOW from the tool's own purpose:**
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

Use `AskUserQuestion` (or plain questions) ONLY for genuine gaps you couldn't infer, e.g.:
- the test/benchmark command if you couldn't find it
- the target threshold for a percentage/cardinal goal
- the token budget and max wall-time, if the user wants guards
- the inner-worker model (default `claude-opus-4-8[1m]`)

Show the user the proposed `goals.yaml` and let them approve or edit before writing.

## Step 6 — Write the three files

Write to the **project directory** (where the user invoked this) — OR, to keep the root tidy,
into an optional **`agg/` config folder**. `agg run` auto-detects either: if `<project>/agg/`
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

### `agg.yaml`
```yaml
project: <name>
model: "claude-opus-4-8[1m]"
resume_prompt: "AGG_RESUME.md"
heartbeat_secs: 30
watchdog: { idle_secs: 900, cpu_grace: 180 }
ratelimit_backoff_secs: 1800
budget: { total: <tokens or null> }
summary: { enabled: true, model: haiku, min_interval_secs: 300 }
# hooks + prompt_includes: ONLY if Step 4.5 wired tools the user confirmed. Omit otherwise.
# hooks:
#   on_start:       ["<build-graph-cmd>"]      # whatever the detected tool needs
#   on_session_end: ["<refresh-cmd>"]
#   background:     ["<watch-cmd>"]            # reaped automatically on stop
# prompt_includes: ["AGG_TOOLING.md"]
```

### `AGG_RESUME.md` (the fat resume prompt — this is the worker's standing instructions)
Write a self-contained prompt that, on EVERY fresh session, tells the worker to:
1. Read its handoff/state (a `HANDOFF.md` you also create, or the project's existing one)
2. Do ONE self-contained chunk of work toward the goals
3. Commit as it goes
4. Before exiting (context fills — `claude -p` does NOT auto-compact): rewrite the handoff
   with new state + the exact next task, commit
5. Be autonomous — there is NO human in the loop; never pause to ask

Inline any skill/workflow content the worker needs (skills are NOT invocable in headless
`-p` — see the harness docs), e.g. paste the relevant GSD execution steps directly.

Also create a starter `HANDOFF.md` capturing the current state + first task.

## Step 7 — Validate the baseline

Run the dry-run to confirm the judges work and show the starting scoreboard:
```bash
agg plan
```
If a judge errors, fix its command/rubric before finishing.

## Step 8 — Tell the user how to launch

```
Setup complete. Starting scoreboard above.

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

(If `agg` is not on PATH, tell them to install it — Homebrew/GitHub Releases — since the
plugin ships only these skills, not the CLI binary.)
