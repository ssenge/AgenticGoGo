# `agg` configuration reference

One file, `agg/agg.yaml`, holds everything: `defaults` / `judge` / `steps` / `sequence` plus a few
top-level survivors. **There is no `goals.yaml`** — a judge IS a goal, resolved by name from disk
(see [Judges](#judges-resolved-by-name)). Every struct is parsed with `deny_unknown_fields`, so a
misspelled or misplaced key is a **hard error at startup**, never a silent no-op. (That guard is what
makes a stray top-level `budget:` — which now belongs under `sequence:` — fail loudly instead of
becoming a decorative spend ceiling.)

The only required keys are `project` and `sequence`. Everything else has a default.

```yaml
project: "my-project"

# Inherited by EVERY step; a step body may override any of these.
defaults:
  agent: claude                    # the WORKER default: claude · codex · copilot
  model: "claude-opus-4-8[1m]"     # None ⇒ the agent's own default (codex: OMIT; copilot: auto)
  effort: high                     # low|medium|high|xhigh|max — None ⇒ backend default; "" ⇒ none
  worker_args: []                  # extra flags passed VERBATIM to the worker (the sandbox constraint)
  state: "AGG_STATE.md"            # the forward state file, resolved against agg/

# THE RULER — runs LLM judges + the summarizer. Run-level and IMMUTABLE: naming any of these keys
# in a step body is a hard error (a grader that moves makes verdicts incomparable across cycles).
judge:
  agent: claude
  model: "claude-haiku-4-5-20251001"   # None ⇒ the ruler's cheap default. A cheap model grades.
  timeout: 300                         # seconds — EVERY judge, script and LLM alike

# The step palette. NAME → a body of OVERRIDES over `defaults:`. The name is your own label.
steps:
  worker: {}                       # pure defaults
  reconsider:                      # (example) a stall-triggered step-back on a different vendor
    agent: codex
    prompt: "Assume the current approach is wrong. Name 2-3 different approaches…"
    skip_judges: true

# The repeating sequence + the run-level ceilings and Definition of Done.
sequence:
  steps:
    - worker x4
    - if stalled then reconsider
  budget: { total: 5000000 }       # output-token ceiling (worker AND judge spend) → over_budget
  cost:   { total: null }          # dollar ceiling → over_cost. CLAUDE-ONLY (null = unlimited)
  max_sessions: 0                  # 0 = unlimited. Backs over_iterations
  gate_regressions: true           # roll a session back if a previously-met judge regresses
  invariants: [no_regression]      # judge names that must STAY met
  done_if: "correct_result AND all_tests_pass AND coverage.value >= 80"
  abort_if: "over_budget OR wall_hours >= 8 OR any_regressed(invariants) OR any_judge_error"

# ---- top-level survivors (all optional) ----
heartbeat_secs: 30
watchdog: { idle_secs: 900, cpu_grace: 180 }
ratelimit_backoff_secs: 1800       # claude + codex; copilot cannot flag a rate-limit
summary: { enabled: true, min_interval_secs: 300 }   # runs on the RULER — no model: here
memory:  { enabled: true, max_kb: 64, inject_kb: 8 }
session_isolation: {}              # MANDATORY; keys: branch_prefix, base_branch, red_file
hooks: {}                          # on_start / on_session_start / on_session_end / on_stop / background
prompt_includes: []                # files prepended to every worker prompt
```

## `defaults` — inherited by every step

| key | default | notes |
|---|---|---|
| `agent` | `claude` | the worker default. `claude` · `codex` · `copilot`. |
| `model` | none | none ⇒ the agent's own default. **Codex: omit** (naming one is a hard 400). **Copilot: `auto`.** |
| `effort` | none | `low\|medium\|high\|xhigh\|max`. none ⇒ backend default; `""` ⇒ pass no effort flag. Codex clamps `max`→`high`. |
| `worker_args` | `[]` | extra flags passed **verbatim** to the worker CLI — the sandbox constraint. Inheritable so you set it once. |
| `state` | `AGG_STATE.md` | the forward state file (see [State](#state-and-memory)), resolved against `agg/`. |

## `judge` — THE RULER

The one agent + model that runs every **LLM** judge and the summarizer. It is a separate, run-level
block precisely because it is immutable: a moving grader makes verdicts incomparable across cycles.
**Naming `agent` / `model` / `timeout` in a step body is a hard error.**

| key | default | notes |
|---|---|---|
| `agent` | `claude` | usually the same as `defaults.agent`, but need not be — a Codex worker with a Claude ruler is valid. |
| `model` | none | none ⇒ the ruler's cheap-model default. |
| `timeout` | `300` | seconds, enforced for **every** judge (script and LLM) with a process-group kill. |

If the ruler is rate-limited or unreachable, the run **parks in backoff and merges nothing** — it
never fails over to another agent (that would move the ruler) and never fabricates a verdict.

## `steps` — the palette

Each NAME maps to a body of overrides. **The complete legal key list** (anything else is a hard error):

| key | default | notes |
|---|---|---|
| `agent` | `defaults.agent` | per-step agent — a different **vendor** is the strongest perspective diversity. |
| `model` | `defaults.model` | per-step model — grunt work on a cheap model, the step-back on a strong one. |
| `effort` | `defaults.effort` | validated against the step's backend. |
| `worker_args` | `defaults.worker_args` | |
| `state` | `defaults.state` | |
| `prompt` | none | **ADDITIVE** to the composed prompt, never replacing it. |
| `skip_judges` | `false` | `true` ⇒ no judges run after this step, so nothing merges — the work **stages** (below). |

**`skip_judges` steps stage.** Nothing was judged, so nothing merges; the work stays on the session
branch and the **next judged step gates the whole span** — pass ⇒ the span merges, a regression ⇒ the
whole span rolls back. A sequence of *only* `skip_judges` steps is refused at startup (nothing could
ever merge).

## `sequence` — the loop

### `sequence.steps` — the statement grammar

A list of statement lines. Each line is one of:

```
NAME                          # run step NAME once
NAME xN                       # run it N times (N ≥ 1), e.g.  worker x4
if <expr> then NAME           # run NAME only when <expr> is true
if <expr> then NAME else NAME # …otherwise run the else step
```

- `<expr>` is the [condition grammar](#the-condition-grammar) below.
- Keywords (`if` / `then` / `else` / `x`) are case-insensitive. **No nesting** — a branch target is a
  single step name.
- An **unknown step name, or a judge name that resolves to no file, is a hard error at startup**,
  listing what does exist. Never a runtime surprise.

The sequence repeats from the top, forever, until `done_if` fires (exit **0**) or `abort_if` fires
(exit **3**). Before session 1, every judge in the run-set runs once against the untouched repo (the
**baseline**), so a run can end immediately as already-done or already-aborting.

### `sequence` keys

| key | default | notes |
|---|---|---|
| `budget: { total }` | unlimited | **output-token** ceiling → `over_budget`. Counts **worker AND judge** spend, summed across all agents. Works on every agent. |
| `cost: { total }` | unlimited | **dollar** ceiling → `over_cost`. **CLAUDE-ONLY** — see [Choosing an agent](#agent-specific-rules). |
| `max_sessions` | `0` | `0` = unlimited. Backs `over_iterations`. A **non-zero** `agg run --max-sessions <n>` overrides it; the flag's default `0` falls back to this key (not to unlimited). |
| `gate_regressions` | `true` | roll a session back if a previously-met judge now fails. The rename of the old `rollback_on_regression`. |
| `invariants` | `[]` | judge names that must STAY met. The gate protects them; `any_regressed(invariants)` gives up on them. |
| `done_if` | `all_goals` | the **Definition of Done** — success stop (exit 0). |
| `abort_if` | none | the giving-up guard (exit 3). |

## The condition grammar

`done_if`, `abort_if`, and every `if` condition use one whitelisted boolean grammar (there is no
second expression language). Operators: `AND` / `OR` / `NOT` (word or symbolic `&& || !`), the
comparisons `== != >= <= > <`, and parentheses. Precedence: `or > and > cmp > atom`.

**Terms:**

| kind | terms |
|---|---|
| **judge (bare name)** | any judge name → its `met` **bool**. |
| **judge accessor** | `name.value`, `name.max` → the number the judge emitted. (`.target` is NOT an accessor — it is presentational.) |
| **aggregates** | `all_goals`, `count_met`, `count_regressed`, `total`, `met_fraction`, `any_regressed` |
| **run scalars/ceilings** | `tokens_spent`, `budget_total`, `over_budget`, `cost_spent`, `cost_limit`, `over_cost`, `iterations`, `max_iterations`, `over_iterations`, `wall_hours`, `any_judge_error` |
| **invariant subset** | `(invariants)` — an argument on exactly `count_met`, `count_regressed`, `total`, `met_fraction`, `any_regressed`, e.g. `any_regressed(invariants)`. |

### Numeric thresholds — use the accessor

```yaml
done_if: "tests_pass AND coverage.value >= 80"   # ✅  read the number the coverage judge emitted
done_if: "tests_pass AND coverage >= 80"         # ✗  HARD ERROR — a judge name is a BOOL
```

A bare judge name is its `met` boolean; comparing a bool to a number is meaningless, so agg **refuses
it at startup** and tells you to use `coverage.value >= 80`. A threshold has one owner — the condition
— so the judge's own `target` is presentational only (it drives progress bars, nothing more).

### The two quantifiers — the DoD-set vs the run-set

- **The RUN-SET** = every judge named in `done_if` ∪ `abort_if` ∪ `invariants:` ∪ every `if`
  condition. These are the judges that actually execute after each step. `any_judge_error` ranges over
  this set. (This is why a `stalled` judge used only in `if stalled then …` runs, without being listed
  anywhere else.)
- **The DoD-set** = judges named in `done_if` ∪ `invariants:` only. The aggregates (`all_goals`,
  `count_met`, `total`, `met_fraction`, `any_regressed`) range over **this** set — and it is what the
  scoreboard's `N/M` counts.

They differ deliberately: if `all_goals` ranged over the run-set, `done_if: all_goals` could not be
true until `stalled` was met — i.e. the loop would "succeed" by getting stuck. **Never put a judge
that only appears in an `if` branch into `done_if`.**

### `abort_if` is not part of the DoD

Done is one thing; giving up is another. `abort_if` is a ceiling (budget, time, a regressed
invariant, a judge error) that exits **3** so automation can tell a guardrail bail from a real win.
Typical values:

```yaml
# any agent:
abort_if: "any_regressed(invariants) OR over_budget OR over_iterations OR wall_hours >= 8"
# claude only — add the dollar ceiling:
abort_if: "any_regressed(invariants) OR over_cost OR over_budget OR over_iterations OR wall_hours >= 8"
```

Do **not** fold a ceiling into `done_if` — putting `over_budget` there would report a blown budget as
*success*. **Never leave an autonomous loop with no ceiling at all.** `any_judge_error` is `true` when
a judge that ran this step crashed / timed out / emitted garbage — an `error` is never a regression
and never satisfies `done_if`; wiring `abort_if: … OR any_judge_error` is the explicit policy.

## Judges resolved by name

A judge NAME in a condition resolves to a **file on disk** — there is no registry, no `kind:` tag, no
`cmd:`:

```
coverage
  1. agg/judges/coverage.sh   → a script judge         (THIS project's — shadows everything)
  2. agg/judges/coverage.md   → an LLM judge; the FILE IS THE RUBRIC
  3. ~/.agg/judges/coverage.* → the STANDARD LIBRARY
  4. else → HARD ERROR AT STARTUP, listing every available judge name
```

**The extension decides the kind.** `.sh` = script, `.md` = rubric ⇒ LLM.

A judge prints a **verdict JSON** to stdout (agg reads the *last* JSON object, so it may log freely):

```jsonc
{
  "met":       true,        // required — did this clause pass?
  "value":     83,          // optional — a count/percent (drives progress + the .value accessor)
  "max":       100,         // optional — the denominator
  "target":    80,          // optional — presentational only
  "rationale": "one line",  // optional — shown on the dashboard
  "evidence":  ["…"]        // optional — the judge's citations, persisted
}
```

A judge that emits no `value` is treated as **binary**; one that emits a `value` shows partial
progress. A judge that exits non-zero but prints a valid verdict is accepted. A judge that
crashes / times out / prints garbage is an **`error`** — never counted as a regression.

### Script judges

Run from the **project root** (cwd = project root, stdin = `/dev/null`), with env `AGG_SESSION`,
`AGG_STEP`, `AGG_JUDGE`, `AGG_PROJECT_DIR` set. Just a file that prints the verdict:

```bash
#!/usr/bin/env bash
# agg/judges/coverage.sh
pct=$(coverage report | awk '/TOTAL/ {print $NF}' | tr -d '%')
met=$([ "$pct" -ge 80 ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":100,"target":80,"rationale":"%s%% covered"}\n' "$met" "$pct" "$pct"
```

### LLM (rubric) judges

An `.md` file **is** the rubric. It declares the files it reads in its own YAML **frontmatter**; the
body is the criteria. It runs tools-off on the RULER, with the repo's content fed as *untrusted data*
the judge is told never to obey. The judge model is the `judge:` block's — nothing model-specific goes
in the `.md`.

```markdown
---
inputs: ["diff", "src/solver.rs", "log:logs/test.out"]
---
Grade the diff against these criteria… Output ONLY the verdict JSON on the last line.
```

Valid `inputs` tokens: `"diff"`, `"diff:<rev>"`, `"status"`, `"log:<path>"` (a tail), or a file path.

### The embedded standard library

A set of **parameterless** judges ships **inside the agg binary** and installs to `~/.agg/judges/` on
`agg init` (and on `agg run` if a file is missing or has drifted from the embedded copy). Install agg →
the judges are installed; update agg → they're updated. No network, no `agg judges` subcommand.

```
cargo_test  build_ok  lint_clean  git_clean  no_regression  stalled  cmd_exit  grep_count
```

Name any of them in a condition and it just resolves — no file needed. To customise one, copy it into
your `agg/judges/<name>.sh`; a project file **shadows** the library by name. Anything that needs an
argument is a three-line script in your own `agg/judges/` — library judges take no parameters.

`stalled` is the stall detector used by `if stalled then …`: it reads the verdict history and is
`met` when the last few merged steps changed no judge's `met` and no numeric judge's `value`.

## Choosing an agent

`agent:` (in `defaults` and `judge`, and overridable per step) picks which coding agent runs. They
are **not interchangeable**, and a config that asks for something an agent can't do is **refused at
startup** — checked for **every** agent the sequence names. The full matrix is in the README under
[Choosing an agent](../README.md#choosing-an-agent).

### Agent-specific rules

| | rule |
|---|---|
| `model:` | **Codex: omit it** (naming a model you aren't entitled to is a hard 400). **Copilot: `auto`.** Applies to both `defaults.model` and `judge.model`. |
| `effort:` | **Copilot cannot combine `effort:` with `model: auto`** (its default) — agg refuses the pair. Codex clamps `max`→`high`. |
| `cost:` / `over_cost` | **Claude only.** Codex reports no dollars; Copilot bills in AI Credits. **Checked per step** — even one `agent: codex` step makes a `cost:` guard uncoverable, so agg refuses it. Use `sequence.budget:` (tokens); Copilot can self-cap with `worker_args: ["--max-ai-credits", "50"]`. |

## Session isolation (mandatory) and the gate

Every session **always** branches off `base`, does its work, and is gated — there is no master switch.
`agg run` therefore **refuses to start** without a git repo, a clean tree, and a non-detached HEAD.
The gate rule: **auto-accept a session's work, UNLESS a judge that was previously met now fails — then
roll that session back** (`gate_regressions: true`). Three things can suppress a merge, in this
precedence:

1. **`red_file`** (`.agg_red` at the project root) — the worker's own veto. Present ⇒ do not merge.
2. **`skip_judges`** — nothing was judged, so nothing merges; the work stages.
3. **the regression gate** — a previously-met judge now fails ⇒ roll back.

`session_isolation` surviving keys: `branch_prefix` (default `agg`), `base_branch` (default: the
current branch), `red_file` (default `.agg_red`).

## Hooks and prompt includes

`agg` is tool-agnostic: it runs *your* shell commands at lifecycle moments and prepends *your* text to
the worker prompt. Use this for a code-graph builder, a memory cache, a linter — whatever you use.

```yaml
hooks:
  on_start:         ["mytool build ."]      # once at startup
  on_session_start: ["mytool refresh ."]    # before each RUN
  on_session_end:   ["mytool persist ."]    # after each VERIFY
  on_stop:          ["mytool export ."]     # once when the loop stops
  background:       ["mytool --watch ."]    # long-lived; reaped automatically on stop
prompt_includes:
  - "AGG_TOOLING.md"                        # your text, prepended to every worker prompt
```

A failing hook is logged, never fatal. `background` processes are spawned in the loop's reaping
domain, so a `--watch` can't leak.

## What the worker can do — and constraining it

`RUN` launches the worker with that agent's auto-approve flag (`claude
--dangerously-skip-permissions`, `codex --dangerously-bypass-approvals-and-sandbox`, `copilot
--allow-all-tools`): a headless agent can't answer permission prompts, so it needs full tool access —
which means **the worker runs with your user's full host access**. The outer loop's rails (watchdog,
budget/cost ceilings, git isolation, the rollback gate) guard the *loop*; they do not sandbox the
agent itself. For unattended overnight runs, prefer a container/VM you're willing to hand to an
autonomous agent.

Narrow what the worker may do with `worker_args` (passed **verbatim**, so use that agent's own
vocabulary). Pick the ONE line for your agent:

```yaml
worker_args: ["--allowedTools", "Edit,Bash", "--add-dir", "src"]   # claude
worker_args: ["--sandbox", "workspace-write"]                       # codex
worker_args: ["--max-ai-credits", "50"]                            # copilot
```

The judge and summarizer always run as separate **read-only** calls loading only your own settings —
never the agent-mutated repo's config — so the worker cannot steer the thing that grades it. Same
guarantee, three mechanisms: Claude `--strict-mcp-config` + `--setting-sources user`; Codex `--sandbox
read-only`; Copilot by withholding `--allow-all-tools`.

## State and memory

Everything agg **reads** is under `agg/` (committed); everything it **writes** is under `agg/state/`
(gitignored, auto-created). One folder, one rule.

- **`agg/AGG_STATE.md`** — the forward state file (`what to do next`). The **agent** maintains it
  best-effort; agg warns loudly if a session leaves it untouched.
- **`agg/state/AGG_MEMORY.md`** — durable institutional memory (`what we tried and rejected`). Written
  by **agg**, never the worker — never tell the worker to maintain it.
- **`agg/state/state.json`** — the live scoreboard snapshot (the TUI, `agg serve`, `/agg:status` read it).
- **`agg/state/verdicts.jsonl`** — the append-only, safety-critical GATE record.
- **`agg/state/run.pid` · `run.log`** — the loop's liveness and its detached log.
- **`agg/state/bus/`** — the steering queue (`agg send …` writes here; the loop drains it at `INJECT`).

`memory:` keys: `enabled` (default true), `max_kb` (cap on the stored file), `inject_kb` (how much is
injected per prompt). `0` for either disables that cap.

## Environment overrides (CI-friendly)

These override the config at load time:

| env var | overrides |
|---|---|
| `AGG_MODEL` | `defaults.model` (not a step that names its own model) |
| `AGG_COST_TOTAL` | `sequence.cost.total` |
| `AGG_HEARTBEAT_SECS`, `AGG_WATCHDOG_IDLE_SECS`, `AGG_WATCHDOG_CPU_GRACE`, `AGG_RATELIMIT_BACKOFF`, `AGG_MEMORY_MAX_KB`, `AGG_MEMORY_INJECT_KB` | the matching top-level keys |

> **Platform note.** `agg` is **unix-first** (macOS + Linux). The Windows binary builds and the core
> outer loop runs, but two safety features are **not** implemented there: the **CPU-flat half of the
> watchdog** and **process-group reaping**. `agg run` prints a one-line notice on Windows.
