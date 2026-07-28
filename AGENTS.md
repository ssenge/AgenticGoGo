# AGENTS.md — AgenticGoGo (`agg`)

Condensed, imperative reference for AI agents. Humans: read `README.md`. Full config: `docs/CONFIG.md`.
Design specs: `internal/` (gitignored). This file covers what an agent needs to **use** agg (drive the
CLI, write judges, configure a run) and to **work on** this repo.

## What agg is
An autonomous **outer loop** that drives a coding agent (Claude Code / OpenAI Codex / GitHub Copilot) in
a **fresh session every cycle** until a Definition of Done — expressed as **judges** — is met. The
harness is deterministic Rust; the worker is the one stochastic step. **A judge IS a goal.**

Loop: **INJECT** (compose the worker's brief) → **RUN** (one fresh worker on its own git branch) →
**VERIFY** (agg runs the judges itself, externally) → **GATE** (merge the branch if judges pass, else
roll back). Repeat until `done_if` (exit 0) or `abort_if` (exit 3).

## CLI (run from the project root)
- `agg init [--agent claude|codex|copilot] [--force]` — scaffold `agg.yaml` + `AGG.md` + `state/STATE.md` + a starter judge.
- `agg plan` — dry run: evaluate every judge once, print the starting scoreboard.
- `agg run [--max-sessions N] [--detach]` — run the loop until `done_if`/`abort_if`.
- `agg status [--json]` — live scoreboard (reads `state.json`; cheap, does NOT re-judge).
- `agg dashboard [--once]` — live TUI (or one-shot snapshot).
- `agg doctor` — check the chosen agent is installed and the config is valid for it.
- `agg judge <name>` — run ONE judge and print its verdict (author/debug a judge).
- `agg history [--json]` — run history + lifetime totals.
- `agg send inject "<text>" | pause | resume | budget <n> | stop | note "<text>"` — steer a RUNNING loop.
- `agg stop` — graceful stop (alias of `send stop`).
- `agg spawn --name <n> --reason "<why>" -- <cmd…>` — a long task that outlives one worker session.
- `agg skills install [--agent <a>] [--user]` — install the `/agg:*` skills where the agent finds them.
- `agg serve` — JSON HTTP API for the web UI.

## Config: `agg.yaml` (one file — full reference in `docs/CONFIG.md`)
**RULE: quote every string scalar** — `agent: "claude"`, never `agent: claude`. Bare bools/ints stay unquoted.

```yaml
project: "my-project"
defaults:                        # inherited by every step; a step may override any of these
  agent: "claude"                # claude | codex | copilot
  model: "claude-opus-4-8[1m]"   # codex: OMIT the key (naming a model is a hard 400); copilot: "auto"
  effort: "max"                  # low|medium|high|xhigh|max. DEFAULTS: claude max, codex high, copilot none.
                                 # codex clamps xhigh/max -> high; copilot cannot combine effort with model:"auto".
  state: "state/STATE.md"        # worker-curated forward-advice file (under gitignored agg/state/)
judge:                           # THE RULER — runs .md judges + the summarizer. Immutable across a run.
  agent: "claude"
  model: "haiku"
  timeout: 300
steps:                           # palette. legal keys: agent model effort worker_args state role_prompt prompt skip_judges
  worker: {}
sequence:
  steps: ["worker"]              # e.g. "worker x4", "if stalled then reconsider"
  limits: { tokens: null, cost: null, sessions: null }   # null = unlimited
  invariants: []                 # judge names that must STAY met
  done_if: "tests_pass"          # SUCCESS (exit 0)
  abort_if: "over_budget OR over_iterations OR wall_hours >= 4"   # GIVE UP (exit 3)
  notify_if: "stalled"           # OPTIONAL, NON-TERMINAL: fire notify.cmd, KEEP RUNNING
  notify: { cooldown_sessions: 3, cmd: ["curl -s --max-time 10 -d {{reason}} ntfy.sh/my-topic"] }
```

### `done_if` / `abort_if` / `notify_if` grammar
Bare judge names combined with `AND` / `OR` / parens, plus these terms: `all_goals`, `count_met >= N`,
`over_budget` (tokens), `over_cost` (**claude-only**), `over_iterations`, `wall_hours >= N`, `stalled`,
`any_regressed(invariants)`, `count_regressed >= N`.

`notify_if` is the NON-TERMINAL twin of `abort_if` — same grammar, but true ⇒ run `notify.cmd` (like a
hook: best-effort, non-fatal, in the current step's isolation tier, and FOREGROUND + UNTIMED — so bound
every command, `curl --max-time 10`) and
the loop CONTINUES. Its judges join the run-set, never the DoD-set. `{{reason}}` (the rationale of a
judge named in the expression: `met` first, then highest `value`) / `{{project}}` / `{{session}}` /
`{{step}}` are substituted **shell-quoted by agg** — never quote a placeholder yourself.
`cooldown_sessions` (default 3) debounces; an `abort_if` halt pings once ignoring it — including an
`abort_if` already true at launch; `done_if` success never pings (that is `hooks.on_stop`). `notify_if`
with an empty `notify.cmd` is a hard startup error; `notify:` alone is valid and means "ping only when
`abort_if` halts". **THE MOAT: put WORKER-AUTHORED signals (a `blocked` judge over a worker-written
file) in `notify_if`, never `abort_if` — otherwise the agent can end its own run.** `stalled`/`stuck`
read agg's `verdicts.jsonl`, which is a PROTOCOL boundary, not a permission one — it sits in the
worker's writable cwd on every isolation tier. Only the process-internal terms (`over_budget`,
`over_cost`, `over_iterations`, `wall_hours`, `any_regressed(invariants)`) are unfakeable.
Do NOT name the SAME detector in `notify_if` and an `if <expr> then …` branch: the branch is resolved
at the start of the next session, so the human is paged before the recovery step runs.

## Writing a judge (a judge IS a goal)
A judge lives at `agg/judges/<name>.{sh,md}` and is referenced by its **bare NAME** in
`done_if`/`abort_if`/`invariants`/`if`-conditions.
- **`.sh`** = SCRIPT judge — runs and prints ONE line of verdict JSON to stdout. Runs from the PROJECT
  ROOT (sees the worker's files). `chmod +x` it.
- **`.md`** = LLM RUBRIC judge — agg sends it to the ruler as a READ-ONLY one-shot; it must return the same JSON.

Verdict contract (to stdout):
```json
{"met": true, "value": 3, "max": 3, "target": 3, "rationale": "3/3 tests pass"}
```
`met` (bool) is the gate. `value`/`max`/`target` are numeric progress (used by `count_met`, deltas, the
scoreboard). `rationale` is a short human string.

Env agg sets when running a judge: `AGG_SESSION`, `AGG_STEP`, `AGG_JUDGE`, `AGG_PROJECT_DIR`.

Name resolution: `agg/judges/<name>` first, then the standard library `~/.agg/judges/<name>` (which ships
`cargo_test`, `build_ok`, `lint_clean`, `git_clean`, `no_regression`, `stalled`, `cmd_exit`).

Example — `agg/judges/tests_pass.sh`:
```sh
#!/usr/bin/env bash
n="$(pytest -q 2>/dev/null | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+' || echo 0)"
[ "$n" -ge 1 ] \
  && printf '{"met":true,"value":%s,"max":%s,"target":1,"rationale":"%s pass"}\n' "$n" "$n" "$n" \
  || printf '{"met":false,"value":0,"max":1,"target":1,"rationale":"none pass"}\n'
```

## File layout
```
agg/AGG.md                   COMMITTED  standing project instructions the worker reads (the CLAUDE.md-analog)
agg/agg.yaml                 COMMITTED  the loop config
agg/judges/<name>.{sh,md}    COMMITTED  the Definition of Done  ← THE MOAT: judges MUST stay committed
agg/state/                   GITIGNORED all runtime state:
  INSTRUCTIONS.md              the worker's whole `-p` (agg REGENERATES it every session)
  STATE.md                     worker-curated forward advice (rewritten wholesale each session)
  LOG.md                       durable institutional memory (agg-owned; worker never writes it)
  wiki/                        worker-owned OKF knowledge base — durable; MULTI-SESSION PLANS live here
  sessions/  verdicts.jsonl  state.json  project.json  run.{pid,log}
```

## The three "agent" surfaces — do NOT conflate
- **The worker** (agg's inner agent) reads `agg/AGG.md` + the agg-composed `agg/state/INSTRUCTIONS.md`.
- **An agent OPERATING agg** uses the CLI above + the `/agg:*` skills (`agg:new` / `agg:status` / `agg:supervise`).
- **This file** is the condensed reference for an agent working in/with this repo.

## Working ON this repo (contributing to agg itself)
- **Gate before any commit:** `cargo build` (0 warnings) · `cargo test` · `cargo clippy --all-targets -- -D warnings` · `bash scripts/e2e.sh` (~12 min).
- **Run `cargo test` / e2e OUTSIDE the sandbox** (they SIGKILL processes + bind sockets; a sandboxed e2e gives false failures). If `rtk` rewrites cargo output, use `rtk proxy cargo …`.
- **THE MOAT — never break it:** gitignore ONLY `agg/state/`, NEVER all of `agg/`. The judges must stay committed so a rollback can restore a grader a worker tampered with.
- **agg owns ALL git; the worker NEVER runs git** — the worker just edits files. agg auto-commits its work on the session branch (a `GitAutoCommit` handler on `on_verify`), then merges/keeps or rolls back. The worker never adds/commits/merges/pushes. (GIT_REDESIGN.)
- **Verify agent behavior on the WIRE** (`scripts/e2e_real.sh`), never from docs — agent CLIs differ (Codex's `-p` is `--profile`; only Claude reports dollar cost; etc.).
- **Design specs live in `internal/`** (gitignored): `SEQUENCES.md`, `STATE_REDESIGN.md`.
