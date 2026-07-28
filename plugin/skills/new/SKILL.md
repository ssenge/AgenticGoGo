---
name: agg-new
description: Set up AgenticGoGo for the current project — read existing plans, then generate agg/agg.yaml (defaults/judge/steps/sequence), the judge files under agg/judges/, agg/AGG.md (stable scope) and agg/state/STATE.md (forward advice) so `agg run` can drive the work to completion. Use when the user wants to turn a plan/spec/roadmap into an autonomous agent loop.
disable-model-invocation: false
---

# /agg:new — set up an AgenticGoGo loop for this project

You are setting up **AgenticGoGo** (`agg`): a loop that runs fresh headless agent workers until a
**Definition of Done** — expressed as judges — is met. Your job in this skill is to turn whatever
planning material already exists into the files the loop reads, all under the mandatory `agg/` folder:

- `agg/agg.yaml` — the whole config: `defaults` / `judge` / `steps` / `sequence`
- `agg/judges/<name>.sh` (a script judge) or `agg/judges/<name>.md` (an LLM rubric judge) — one per
  clause of the Definition of Done. **A judge IS a goal.**
- `agg/AGG.md` — the stable scope/goals/architecture each worker reads for orientation (committed)
- `agg/state/STATE.md` — the forward "what to do next" advice the worker rewrites each session
  (gitignored; agg composes it + AGG.md + memory into a per-session `agg/state/INSTRUCTIONS.md` brief)

**Core principle: do NOT replicate spec tooling.** Read what's already there and *translate* it into
judges. Only ask the user for what's genuinely missing.

**The model in one sentence: your judges ARE the gate — your Definition of Done, made executable.**
Each judge decides one clause; `done_if` composes them into "done"; agg runs the judges, never the
worker, so the loop cannot fake its own completion.

`agg` can drive **Claude Code, OpenAI Codex, or GitHub Copilot** — chosen by the `agent:` keys you
write into `agg.yaml`. They are **not interchangeable**, and `agg` REFUSES to start a run whose config
asks for something the chosen agent cannot do. **Step 0 is therefore not optional: get the agent
right, or the config you generate will not start.**

---

## Step 0 — Pick the agent the loop will drive (do this FIRST)

This decides which agent runs the *inner workers*. **There is no default. Do NOT guess, and in
particular do NOT fall back to `claude` just because it appears first in the examples below** — a
`claude`-shaped config handed to a Copilot user silently drives the wrong agent.

**0a. Identify yourself by RUNNING this — you cannot tell from introspection alone:**

```bash
if [ -n "$COPILOT_CLI" ];      then echo copilot
elif [ -n "$CODEX_THREAD_ID" ]; then echo codex
elif [ -n "$CLAUDECODE" ];      then echo claude
else echo "UNKNOWN — ask the user"; fi
```

(Each agent exports its own marker: `COPILOT_CLI=1`, `CODEX_THREAD_ID=<uuid>`, `CLAUDECODE=1`.
If you are nested inside another agent, more than one may be set — the FIRST match above wins,
innermost first.)

**0b. See what is actually installed:**

```bash
claude --version; codex --version; copilot --version
```

**0c. Decide.** The agent running this skill is the sane default for the loop to drive, so propose
the one Step 0a printed — but they are independent choices (you can drive Codex from Claude Code),
so let the user override. **If 0a printed `UNKNOWN`, ASK. Never assume.**

Write the worker default as `defaults.agent: claude|codex|copilot` in `agg.yaml`. Then obey the
matrix below.

### The capability matrix — VERIFIED, do not re-derive

| | claude | codex | copilot |
|---|---|---|---|
| script judges | ✅ | ✅ | ✅ |
| LLM judges (an `.md` rubric judge) | ✅ | ✅ | ✅ |
| summaries (`summary.enabled`) | ✅ | ✅ | ✅ |
| token budget (`sequence.limits.tokens`, `over_budget`) | ✅ | ✅ | ✅ |
| thinking effort (`effort:`) | ✅ | ✅ (clamps `max`→`high`) | ⚠️ **not with `model: auto`** |
| rate-limit backoff | ✅ | ✅ | ❌ |
| **dollar cost (`sequence.limits.cost`, `over_cost`)** | ✅ | ❌ | ❌ |

### Four hard rules. Break one and the config you write CANNOT START.

1. **`sequence.limits.cost` and `abort_if: over_cost` are CLAUDE-ONLY.** Codex and Copilot cannot
   price a session in dollars, so `agg` refuses the config outright rather than let a spend guard
   silently never fire. **This is checked per step:** even one `agent: codex` step in an otherwise
   Claude sequence makes a `cost` guard uncoverable — use **`sequence.limits.tokens` (output
   tokens)** instead, which works on all three. (Copilot can additionally cap itself with
   `--max-ai-credits <n>` via `worker_args`.)
2. **Codex: OMIT `model:` entirely** unless the user explicitly names one. Guessing (e.g.
   `gpt-5-codex`) is a hard 400 — *"not supported when using Codex with a ChatGPT account"*. Which
   models exist depends on how the user authenticated, so let `agg` pick its default.
3. **Copilot: `model: auto` is safe — but then do NOT set `effort:`.** Copilot refuses the pair
   (*"Model `auto` does not support reasoning effort configuration"*) and every worker session dies
   instantly having spent 0 tokens. Use `model: auto` with **no** `effort:` (the default), or name a
   concrete model if the user really wants an effort level.
   Claude: `model: "claude-opus-4-8[1m]"` is the default.
4. **Finish by running `agg doctor`** (Step 7). It re-checks every rule above against **every agent
   the sequence names**. It is a free correctness check — if it passes, the config starts.

> ⚠️ **The failure this ordering exists to prevent:** an early version of this skill said "default
> to the agent you are running in", and Copilot — unable to introspect its own identity — wrote
> `agent: claude` with a `claude-opus` model. `agg doctor` passed (a Claude config IS valid), and
> the loop would have silently driven **Claude instead of Copilot**. `agg doctor` cannot catch this
> — it has no way to know which agent you *meant*. **Step 0a is the only thing that catches it.**

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

## Step 2 — Derive the judges (each is one clause of "done")

From the plans, derive a small set of **concrete, checkable judges** (aim for 3–8). Each judge is a
single clause of the Definition of Done. For each, decide:

- **name**: short snake_case (e.g. `tests_pass`, `modules_migrated`, `api_documented`). **This name
  IS the filename** and how `done_if` refers to it — `tests_pass` → `agg/judges/tests_pass.sh`.
- **kind**: `script` (a `.sh` file) when the check is mechanical, or `llm` (a `.md` rubric file) when
  it's qualitative. **The file extension decides** — there is no `kind:` tag any more.
- **the check**: for a script, the command that decides met/not-met; for a rubric, the criteria.
- **numeric or binary**: if it should show partial progress (`18/28`, `82%`), have the script emit
  `value`/`max`/`target` in its verdict. A judge that emits no `value` is treated as binary.

Mark any **soundness/invariant** judge (things that must STAY true — "never break the build", "no
wrong results") for the `invariants:` list you'll write in Step 4 — not with a per-judge flag.

**There is no `recheck:` any more.** Every judge in the run-set runs after every step. To skip
judging on a purely-exploratory step, that STEP sets `skip_judges: true` (Step 4.5) — the lever is
per-step, not per-judge.

## Step 3 — Write the judge files

Every judge is a **file whose name is the judge name**, emitting a verdict JSON to stdout:
`{"met": <bool>, "value": <num>, "max": <num>, "target": <num>, "rationale": "<one line>"}`
Only `met` is required. `agg` reads the **last** JSON object on stdout, so a judge can log freely.

Two kinds, chosen by extension:

- **script → `agg/judges/<name>.sh`** (preferred when measurable). Its stdout is the verdict JSON.
  It runs **from the project root**, with cwd = project root and these env vars set: `AGG_SESSION`,
  `AGG_STEP`, `AGG_JUDGE`, `AGG_PROJECT_DIR`. Suggest a real command for THIS project (a test runner,
  a benchmark, a coverage tool, a grep-count) and wrap it:
  ```bash
  #!/usr/bin/env bash
  # agg/judges/tests_pass.sh
  out="$(pytest -q 2>&1)"
  passed=$(printf '%s' "$out" | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+' || echo 0)
  failed=$(printf '%s' "$out" | grep -oE '[0-9]+ failed' | grep -oE '[0-9]+' || echo 0)
  total=$(( passed + failed ))
  met=$([ "$failed" -eq 0 ] && [ "$total" -gt 0 ] && echo true || echo false)
  printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s/%s pass"}\n' \
    "$met" "$passed" "$total" "$total" "$passed" "$total"
  ```
  `chmod +x` it. A judge that exits non-zero but prints a valid verdict is still accepted.
- **llm → `agg/judges/<name>.md`** (for qualitative goals) — **the file IS the rubric.** It declares
  the files it reads in its own YAML **frontmatter**, and its body is the grading criteria. It runs
  tools-off on the RULER (the `judge:` block), so a repo file can't talk the judge into a pass.
  ```markdown
  ---
  inputs: ["diff", "log:logs/test.out", "src/main.rs"]
  ---
  Grade the artifact against these criteria: …

  Output ONLY the verdict JSON on the last line:
  {"met": <bool>, "value": <0..1>, "max": 1, "target": 1, "rationale": "<one line>"}
  ```
  Valid `inputs` tokens: `"diff"`, `"diff:<rev>"`, `"status"`, `"log:<path>"` (tail), or a file path.
  The LLM judge's model comes from the **`judge:` block** (Step 6), not the rubric — so it is set once
  for the whole run, on the RULER. (Nothing model-specific goes in the `.md`.)

**Reuse the standard library.** A set of parameterless judges ships inside the binary and installs to
`~/.agg/judges/`: `cargo_test`, `build_ok`, `lint_clean`, `git_clean`, `no_regression`, `stalled`,
`cmd_exit`, `grep_count`. If one fits, just name it in `done_if` — no file needed. To customise it,
copy it into `agg/judges/<name>.sh` (a project file shadows the library by name). Anything needing an
argument is a three-line project script.

## Step 4 — Compose the Definition of Done (`done_if`, `abort_if`, `notify_if`, `invariants`)

These live under `sequence:` (Step 6). All use the same whitelisted expression grammar over judge
**names** (NOT arbitrary code):

- **`done_if`** — the success condition (exit 0). This IS your Definition of Done. A **bare judge
  name** is its `met` bool; compose with `AND`/`OR`/`NOT` and parentheses. Aggregates: `all_goals`,
  `count_met`, `total`, `met_fraction`. Default: `all_goals`.
  - **Numeric thresholds use the dotted accessor** — `coverage.value >= 80`, NOT `coverage >= 80`.
    A bare name is a bool; comparing a bool to a number is a **hard error at startup** (agg tells you
    to use the accessor). `.value` and `.max` are the accessors; `.target` is presentational only.
  - Examples: `"tests_pass"` · `"tests_pass AND coverage.value >= 80"` · `"met_fraction >= 0.75"` ·
    `"count_met >= 3"`.
- **`abort_if`** — the giving-up guard (exit 3). NOT part of the DoD — a ceiling, not a definition of
  done. Terms: `over_budget`, `over_cost` (Claude-only), `over_iterations`, `wall_hours`,
  `any_regressed(invariants)`, `any_judge_error`. They OR together — the loop aborts the moment any
  trips.
  ```yaml
  # claude:
  abort_if: "any_regressed(invariants) OR over_cost OR over_budget OR over_iterations OR wall_hours >= 8"
  # codex / copilot — SAME, minus over_cost (they cannot report dollars):
  abort_if: "any_regressed(invariants) OR over_budget OR over_iterations OR wall_hours >= 8"
  ```
- **`notify_if`** — *optional, and the ONLY clause that does not end the run.* Same grammar; when it
  is true agg runs `sequence.notify.cmd` and **the loop keeps running**. A human is a side-channel,
  never a gate — a loop that halts to ask is exactly the babysitting agg exists to remove. Detectors
  are ordinary judges, so there is nothing new to learn.
  ```yaml
  notify_if: "stalled"                              # `stalled` ships in the binary — nothing to author
  notify:
    cooldown_sessions: 3                            # min sessions between pings (default 3; 0 = every cycle)
    cmd: ["curl -s --max-time 10 -d {{reason}} ntfy.sh/my-topic"]   # hook-like: best-effort, never fatal
  ```
  Delivery is **foreground and untimed**, so every command must bound itself (`curl --max-time 10`,
  or `timeout 10 …`) — an unbounded one hangs the loop, the exact thing `notify_if` exists to avoid.
  `{{reason}}` is the rationale of a judge named in the expression (`met` first, then highest
  `value`); `{{project}}` / `{{session}}` / `{{step}}` also work. agg substitutes them
  **shell-quoted**, so **never wrap a placeholder in quotes of your own** — you would get literal
  quote characters. `notify_if` with an empty `notify.cmd` is refused at startup (nothing would
  fire); `notify:` **without** `notify_if` is valid and means "stop + notify" — a ping only when
  `abort_if` halts, including an `abort_if` already true at launch. A `done_if` success never pings;
  for that use `hooks.on_stop`.
  - **⚠ THE MOAT — a WORKER-AUTHORED signal goes in `notify_if`, NEVER in `abort_if`.** A detector
    that reads something the worker wrote (a `blocked` judge over `agg/state/BLOCKED.md`, say) hands
    the agent a way to end its own run by declaring itself stuck — the precise failure the judges
    exist to prevent. In `notify_if` the worst case is an annoying ping, rate-limited by the
    cooldown, and the loop keeps going. Keep TERMINATION on the signals the worker genuinely cannot
    reach — the process-internal ones: `over_budget`, `over_cost`, `over_iterations`, `wall_hours`,
    `any_regressed(invariants)`. A `stalled`/`stuck` detector over agg's own
    `agg/state/verdicts.jsonl` is a **higher bar but not unfakeable**: agg owns that file and a
    worker touching it is tampering, yet `agg/state/` is inside the worker's writable cwd on every
    isolation tier and the ledger has no integrity check.
  - **Do not reuse one detector across clauses.** `notify_if: "stalled"` next to a
    `"if stalled then reconsider"` step pages the human at the gate of the session that just ran,
    one full cycle before the recovery step is dispatched. Either omit the `if` branch, or give
    `notify_if` the stricter detector (`stuck.value >= 85`).
  - **Only propose a detector that resolves.** `stalled` is shipped; `stuck` and `blocked` are judges
    you must WRITE into `agg/judges/` in Step 3 first — naming one that resolves to no file is a hard
    startup error, and `agg doctor` will refuse the config.
- **`invariants:`** — a list of judge names that must STAY met (the soundness guards). The gate rolls
  back any session that regresses one, and `any_regressed(invariants)` in `abort_if` gives up on it.

**Never leave an autonomous loop with no ceiling at all.** If you drop `over_cost` for codex/copilot,
you MUST keep `over_budget` (with a real `sequence.limits.tokens`) in its place. **Never put a judge
that only appears in an `if` branch (like `stalled`) into `done_if`** — the loop would "succeed" by
getting stuck.

## Step 4.5 — Design the steps and the sequence

This is what's new and powerful. A **step** is an `(agent, model, role)` triple; a **sequence** is a
repeating list of statements over those steps. Most projects need just one plain `worker` step. Reach
for more when the run risks going down a rabbit hole.

- **`steps:`** — a palette. Each NAME maps to a body of *overrides* over `defaults:` (legal keys:
  `agent`, `model`, `effort`, `worker_args`, `state`, `role_prompt`, `prompt`, `skip_judges` —
  anything else is a hard error). `prompt:` is ADDITIVE to the composed prompt; `role_prompt:` is a
  generic ROLE framing composed *above* `prompt:` (e.g. a red-team "reconsider" step). `skip_judges:
  true` means no judges run after that step, so nothing merges — its work **stages**, and the next
  judged step gates the whole span.
- **`sequence.steps:`** — a list of statements, each one of:
  - `NAME` — run that step once
  - `NAME xN` — run it N times (e.g. `worker x4`)
  - `if <expr> then NAME [else NAME]` — run a step only when a condition holds; `<expr>` is the
    Step-4 grammar (e.g. `if stalled then reconsider`)

  The sequence repeats from the top, forever, until `done_if` fires (exit 0) or `abort_if` (exit 3).

**The headline pattern — vendor-diverse reconsider.** When a run stalls, a step-back on a *different
vendor* breaks the local optimum better than the same vendor's stronger model — a Claude rabbit hole
is not necessarily a Codex one. And grunt work on a cheap worker + the step-back on a strong one is
the cost argument. Offer this when the project is open-ended/research-shaped:

```yaml
steps:
  worker: {}                       # pure defaults — the grunt worker
  reconsider:
    agent: "codex"                 # a DIFFERENT vendor — perspective diversity
    prompt: >
      Assume the current approach is wrong. Name 2-3 fundamentally different approaches, pick one,
      and write the rejected ones and WHY into your scratch note — agg will persist them.
    skip_judges: true              # stages; the next worker step gates it
sequence:
  steps:
    - "worker x4"
    - "if stalled then reconsider"
```

`stalled` is the library judge (it reads the verdict history). If you use `if stalled then …`, the
`stalled` judge is automatically in the run-set — you don't list it anywhere else. **Only propose a
second agent the user actually has installed** (Step 0b) — `agg doctor` checks every agent named.

## Step 4.7 — Detect the user's tools and offer to wire them in (NO hardcoded tool list)

agg the binary is tool-agnostic — it only runs generic lifecycle hooks. But the worker runs in THIS
user's environment and inherits whatever tools the session has. A worker that USES those tools (a code
graph instead of grepping, a memory tool to recall state across sessions) is cheaper and smarter. So:
**enumerate the tools that are actually active in this session, then ASK the user which to wire into
the loop.** Do NOT assume any specific tool exists — discover them.

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
  worker to recall state at session start and save a handoff note at session end.
- A **token/cost proxy hook** already in global settings → just inform the user it's inherited
  automatically; nothing to configure.
- **Anything else** (a linter, a test-cache warmer, a custom CLI) → ask if they want a hook, and let
  them name the command. The mechanism is identical regardless of the tool.

**Rules:** never invent a tool that isn't present; only offer what you actually detected. Only write
hooks the user confirms. If the user declines all, write no hooks — that's fine. The result goes into
`agg.yaml` (`hooks:` + `prompt_includes:`) and, for prompt guidance, a small `AGG_TOOLING.md` you
reference from `prompt_includes`.

## Step 5 — Ask ONLY what's missing

Ask (with a structured picker if your agent has one, else plain questions) ONLY for genuine gaps
you couldn't infer, e.g.:
- which agent the loop should drive, if Step 0 didn't settle it
- the test/benchmark command if you couldn't find it
- the numeric threshold for a `judge.value >= N` clause
- the token budget and max wall-time, if the user wants guards
- the inner-worker model — but see Step 0 rules 2–3: **never guess one for codex**

Show the user the proposed `agg.yaml` (and the judge files) and let them approve or edit before writing.

## Step 6 — Write the files (all under `agg/`)

Everything agg reads lives under `agg/` (committed); everything it writes lives under `agg/state/`
(gitignored, auto-created). Write:

### `agg/agg.yaml`

```yaml
project: "<name>"

defaults:
  agent: "<claude|codex|copilot>"        # from Step 0 — REQUIRED (the worker default)
  # model: "<model>"                     # claude: "claude-opus-4-8[1m]" · copilot: auto · codex: OMIT
  # effort: <low|medium|high|xhigh|max>  # NOT with copilot's model: auto
  state: "state/STATE.md"                # forward-advice file (under agg/, gitignored)

judge:                                   # THE RULER — runs LLM judges + the summarizer; immutable
  agent: "<claude|codex|copilot>"        # usually the same as defaults.agent
  # model: "<cheap model>"               # claude: a haiku · copilot: auto · codex: OMIT
  timeout: 300                           # seconds, EVERY judge (script + LLM)

steps:
  worker: {}                             # add more only if Step 4.5 designed them
  # a step may also carry `prompt:` (its specific ask) and `role_prompt:` (generic role framing,
  # e.g. a "reconsider" red-team step: { skip_judges: true, role_prompt: "...", prompt: "..." })

sequence:
  steps:
    - "worker"
  limits:                                # run ceilings — omit a key or use null for "unlimited"
    tokens: <int or null>                # output-token ceiling → over_budget. Works on ALL agents.
    # cost: <dollars or null>            # → over_cost. CLAUDE-ONLY, all-Claude sequences only.
    sessions: <int or null>              # worker-session cap → over_iterations (or `agg run --max-sessions <n>`)
  invariants: [<judge names that must STAY met>]
  done_if: "<expression over judge names>"
  abort_if: "<ceiling expression>"       # see Step 4
  # notify_if: "stalled"                 # optional PING that does NOT stop the loop — see Step 4.
  #                                      # `stalled` is shipped; a `stuck`/`blocked` detector must be
  #                                      # AUTHORED in agg/judges/ first, or startup hard-errors.
  # notify: { cooldown_sessions: 3, cmd: ["curl -s --max-time 10 -d {{reason}} ntfy.sh/my-topic"] }

# Optional top-level survivors — omit if unused:
# heartbeat_secs: 30
# watchdog: { idle_secs: 900, cpu_grace: 180 }
# ratelimit_backoff_secs: 1800           # claude + codex; copilot cannot flag a rate-limit
summary: { enabled: true, min_interval_secs: 300 }   # runs on the RULER (no model: here)
# hooks + prompt_includes: ONLY if Step 4.7 wired tools the user confirmed
```

Agent-specific reminders (Step 0): **codex** — no `model:` in `defaults` OR `judge`, no `cost:`.
**copilot** — `model: auto`, no `effort:`, no `cost:`, no `ratelimit_backoff_secs`, optional
`worker_args: ["--max-ai-credits", "50"]`.

### `agg/judges/<name>.sh` and `agg/judges/<name>.md`

Write one file per judge you derived in Step 2 (skip any you're reusing from the library). `chmod +x`
the `.sh` scripts. Reference each by its bare name in `done_if` / `abort_if` / `invariants`.

### `agg/AGG.md` — the stable scope (committed) and `agg/state/STATE.md` — the forward advice

agg regenerates a per-session `agg/state/INSTRUCTIONS.md` brief (the worker's whole `-p` is a tiny
pointer at it) by composing AGG.md + STATE + memory. Split the standing content by change-frequency:

**`agg/AGG.md`** (committed, rare edits, human-owned) — the STABLE scope: what the project is, the
goal, the architecture (key modules/entry points + the exact build/test commands), and the rules:
- Do ONE self-contained chunk of real, correct work toward the judges each session (no stubs).
- Be autonomous — there is NO human in the loop; never pause to ask.
- Inline any workflow content the worker needs — do not assume it can invoke a skill by name in a
  headless session.

**`agg/state/STATE.md`** (gitignored, the worker REWRITES it each session) — the forward "what to do
next": where things stand + the exact next task. Seed it with a first-session note; the worker keeps
it current for its successor.

(Institutional memory, `agg/state/LOG.md`, is written by agg, not the worker — never tell the worker
to maintain it. The worker just edits files and NEVER runs git — agg commits the work automatically,
runs the session on a throwaway branch, and keeps it only if the judges pass; agg owns all git.)

## Step 7 — Validate: `agg doctor`, then `agg plan`

**`agg doctor` first — it is the check that catches a config the chosen agent(s) cannot honour**
(a cost guard on a codex step, a model name codex will reject, a stray `effort:`). It verifies every
agent the sequence names is installed AND that every key you wrote is one that agent can deliver:
```bash
agg doctor
```
Fix anything it marks ✗ — a ✗ here means `agg run` would refuse to start.

Then the dry-run, to confirm the judges resolve and work, and show the starting scoreboard:
```bash
agg plan
```
If a judge errors or fails to resolve, fix its file/name before finishing. (`agg plan` lists every
available judge name on a resolution failure.)

## Step 8 — Tell the user how to launch

```
Setup complete — driving <agent>. Starting scoreboard above.

To run the loop:
  agg run                # foreground, watch it live
  agg run --detach       # background (pidfile + agg/state/run.log), survives the terminal

To watch the dashboard (second terminal):
  agg dashboard

To stop / steer:
  agg stop               # graceful stop at the next session boundary
  agg send inject "…"    # high-priority instruction for the next session

The loop stops when:  <done_if>
```

(If `agg` is not on PATH, tell them to install it — the install script or GitHub Releases, see
the repo README — since these skills ship without the CLI binary.)
