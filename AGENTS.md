# AGENTS.md — AgenticGoGo (`agg`)

Condensed, imperative reference for AI agents. Humans: read `README.md` (short) then `docs/GUIDE.md` (full). Config: `docs/CONFIG.md`.
Rust driver API: `docs/RUST_API.md`.
Design specs: `internal/` (gitignored). This file covers what an agent needs to **use** agg (drive the
CLI, write judges, configure a run) and to **work on** this repo.

## What agg is
An autonomous **outer loop** that drives a coding agent (Claude Code / OpenAI Codex / GitHub Copilot) in
a **fresh session every cycle** until a Definition of Done — expressed as **judges** — is met. The
harness is deterministic Rust; the worker is the one stochastic step. **A judge IS a goal.**

Loop: **INJECT** (compose the worker's brief) → **RUN** (one fresh worker on its own git branch) →
**VERIFY** (agg runs the judges itself, externally) → **GATE** (merge the branch if judges pass, else
roll back). Repeat until `done_if` (exit 0) or `abort_if` (exit 3).

**The three ideas everything here follows from**, IN THIS ORDER — Loop Engineering · Graph
Engineering · Agentic Workflows as Code. They are a ladder, each rung generalising the last. If a
change violates one, it is the CHANGE that is wrong:
- **Loop Engineering.** A FRESH session each cycle beats one long conversation, and the agent NEVER
  decides it is done. Judges do, and the worker cannot run them. (The Ralph loop, made deterministic.)
- **Graph Engineering.** A loop laps in a FIXED order; a graph CHOOSES — a loop is the degenerate
  graph (one node, one back-edge). Generalising it buys conditional work, cost control (order checks
  cheap-to-expensive and short-circuit), recovery paths, and bounds that respond to what the run
  learned. In agg that is the RUST DRIVER: `agg.judge(&cheap).met() && agg.judge(&slow).met()` is a
  real cost gate, not a style choice. ⚠ It is about TOPOLOGY, not memory.
  · **Second sense, different level:** durable knowledge is a graph too — `agg/state/wiki/` is an OKF
  wiki (one concept per file, typed frontmatter, cross-linked), so a fresh session enters at the right
  node. `STATE.md` is rewritten every session; a multi-session PLAN parked there is LOST. It belongs
  in the wiki, which also survives rollbacks.
- **Agentic Workflows as Code.** A graph is a program, so write it in a language built for control
  flow. The whole orchestration is committed source — `agg/agg.yaml` + `agg/judges/*`, diffable and
  reviewable; in Rust, a compiled driver you can unit-test. This is also why the moat holds: judges
  are committed, so a tampered judge is restored by a rollback. ⚠ NOT "Agents as Code" — the agents
  stay agents; it is the WORKFLOW around them that becomes code.

**The bus.** `agg send` steers a RUNNING workflow; with none running it is an ERROR, and stale
commands are purged at start — a queue does not outlive the workflow it was meant for. The rule lives
in `bus::queue_command`, which every channel goes through; it used to be decided separately by the
CLI and the web API, and they disagreed. An ANSWER is not a send: it is a durable fact written to
`agg/private/asks.jsonl` (`agg answer` / `POST /api/answer`), so it works with nothing running.
The loop drains the bus at each session boundary AND every second during a RATE-LIMIT WAIT
(`features/backoff.rs`) — a waiting loop is a running workflow: `wake`/`resume` end the wait (no sooner
than `ratelimit_min_gap_secs`, default 60, after the limited attempt), `stop` ends the run (exit 5), every
other command is re-queued for the next boundary. `wake` outside a wait is a no-op.

## CLI (run from the project root)
- `agg init [--agent claude|codex|copilot] [--force]` — scaffold `agg.yaml` + `AGG.md` + `state/STATE.md` + a starter judge.
- `agg plan` — dry run: evaluate every judge once, print the starting scoreboard.
- `agg run [--max-sessions N] [--detach] [--report-json] [--base-branch <ref>] [--resume]` — run the loop until `done_if`/`abort_if`.
  `--resume` continues the last interrupted run: same `run_id`, session counter (next session = n+1),
  ratchet state and call ledger — truncated back to the last KEPT gate (`features/resume.rs`).
  Every ending writes `agg/private/report.json` (`outcome` done|aborted|stopped|error — authoritative;
  schema `docs/schemas/report.v2.json`; v1 superseded). `--report-json`: logs → `agg/private/run.log`, stdout = the report only.
  The report carries usage per session (`session_log`: worker and ruler apart, the model the agent REPORTED,
  tokens `input`/`output`/`cache_write`/`cache_read`, `cost_usd` + `cost_source` reported|partial|unknown) and totals.
  Exit codes: 0 done · 3 abort_if · 4 max-sessions · 5 stopped (agg stop / SIGTERM / SIGINT) · 1 error.
  Precedence at one session end: error > stopped > done > aborted. `--project <dir>` = `--dir <dir>` (global).
- `agg status [--json]` — live scoreboard (reads `state.json`; cheap, does NOT re-judge). `--json` = the STABLE
  live document (`docs/schemas/status.v1.json`, `src/report/status.rs`): `running`, `current_step`, `session`,
  judges' `last` + `ratchet`, `detectors`, `open_asks`, `usage`, `waiting` — field names match the report.
  ⚠ `state.json` itself is INTERNAL; a new live field goes into `DashboardState` AND the status document + schema.
- `agg dashboard [--once]` — live TUI (or one-shot snapshot).
- `agg doctor [--json]` — check the chosen agent is installed and the config is valid for it. `--json`: a
  versioned report (every backend's path/version/capability flags, isolation tiers available, problems).
- `agg check [--json]` — validate the config EXACTLY as `agg run` startup does, listing ALL problems at once,
  each with a dotted config path (`steps.w.isolation`). Runs no judge/agent. Exit 1 on any problem.
- `agg schema` — print the agg.yaml JSON Schema (generated from the config types; cannot drift).
- `agg judge <name>` — run ONE judge and print its verdict (author/debug a judge).
- `agg history [--json]` — run history + lifetime totals (`--json`: each run with its `usage` totals).
- `agg send inject "<text>" | pause | resume | wake | budget <n> | stop | note "<text>"` — steer a RUNNING loop.
  `wake` ends a rate-limit wait now (`resume` does too while waiting).
- `agg answer <id> <value> [--json]` — answer an open ask (works with nothing running). `--json`:
  `{schema_version, accepted, reason, id, answer}` (`docs/schemas/answer.v1.json`); refused ⇒ exit 1.
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
  write_deny: ["tests/oracles/**"]  # PROTECTED: readable, never changed — gate-checked on EVERY tier
judge:                           # THE RULER — runs .md judges + the summarizer. Immutable across a run.
  agent: "claude"
  model: "haiku"
  timeout: 300
steps:                           # palette. legal keys: agent model effort worker_args state role_prompt prompt skip_judges
                                 #   isolation image readonly writable write_deny role_prompt_file prompt_file
                                 # *_file = the inline key read from a file (rel. to agg/ or absolute) at EVERY
                                 # session start; both forms in one block = startup error; hash → private/prompts.jsonl
  worker: {}
sequence:
  steps: ["worker"]              # entries; e.g. { step: worker, times: 4 } / { step: w, until: X, max: 4 }
  limits: { tokens: null, cost: null, sessions: null }   # null = unlimited
  invariants: []                 # judge names that must STAY met
  ratchet: {}                    # NUMERIC no-regression, e.g. { cc: { direction: "down", tolerance: 0.5, baseline: 12 } }
  groups: {}                     # named judge lists, e.g. { quality: ["sonar", "lizard", "codescene"] } → count_met(quality) >= 2
  merge_requires_improvement: false  # true ⇒ merge only if a DoD judge / done_if group / ratchet IMPROVED, else `no_improvement`
  done_if: "tests_pass"          # SUCCESS (exit 0)
  abort_if: "over_budget OR over_iterations OR work_time >= 14400"  # GIVE UP (exit 3). SECONDS.
  notify_if: "stalled"           # OPTIONAL, NON-TERMINAL: fire notify.cmd, KEEP RUNNING
  notify: { cooldown_sessions: 3, cmd: ["curl -s --max-time 10 -d {{reason}} ntfy.sh/my-topic"] }
```

### `done_if` / `abort_if` / `notify_if` grammar
Bare judge names combined with `AND` / `OR` / parens, plus these terms: `all_goals`, `count_met >= N`,
`over_budget` (tokens), `over_cost` (**claude-only**), `over_iterations`, `stalled`,
`any_regressed(invariants)`, `count_regressed >= N`, a GROUP subset on any aggregate —
`count_met(<group>) >= k` (k of n), `all_goals(<group>)`, `any_regressed(<group>)` …, the gate's own
history — `rollbacks >= N`, `consecutive_rollbacks >= N`, `no_improvements >= N`,
`consecutive_no_improvements >= N`, `oscillating` (A→B→A over the unmet DoD-set, window
`sequence.oscillation_window`, default 4), `ratchet_regressed`, `<judge>.ref` (a ratcheted judge's
reference) — and the three clock terms — all **SECONDS**:
`wall_time` (e2e, human waiting included — a deadline), `human_wait_time`, `work_time`
(`wall_time − human_wait_time` — an effort ceiling, and what a run with humans in it wants).
⚠ `wall_hours` was REMOVED, not renamed: the unit changed by 3600×, so agg hard-errors on the old key
rather than aliasing `wall_hours: 8` into an eight-SECOND ceiling.

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
`over_cost`, `over_iterations`, `wall_time`/`human_wait_time`/`work_time`, `any_regressed(invariants)`,
`rollbacks`, `consecutive_rollbacks`, `no_improvements`, `consecutive_no_improvements`, `oscillating`,
`ratchet_regressed`, `<judge>.ref`) are unfakeable —
the gate's history is computed in agg's memory and only persisted (`agg/private/gate.json`) for resume.

**The ratchet** (`sequence.ratchet.<judge>: { direction: up|down, tolerance, tolerance_pct?, baseline? }`):
a ratcheted judge's `value` may not get worse than at the last KEPT gate beyond tolerance, else the
session is a gate regression (rolled back). Reference before the first keep: the `baseline:` seed, else
the baseline pass. A ratcheted judge with no `value` refuses at startup; one that errors never passes.
It joins the run-set, never the DoD-set. Driver: `Agg::ratchet(name, Ratchet::down(0.5))`, read with
`agg.gate_state()`.
**Groups** (`sequence.groups.<name>: [judges]`): a NAME for a judge list, usable only as an aggregate
subset. A group is its members — all join the run-set; a group in `done_if` puts them in the DoD-set.
At the gate a judge held ONLY through a `done_if` group is judged with the group, NET (more members
met→unmet than unmet→met = regression), so a swap inside k-of-n is not a rollback. Refused at startup:
unknown member, empty group, duplicate member, reserved name, unknown group in a condition, literal k > n.
Code: `src/core/groups.rs` (`assess` = the one rule both gates call). Driver: `Group::new` + `agg.group(&g)`.
**`merge_requires_improvement: true`**: a staged session merges only if a DoD judge went unmet→met (a
`done_if` group: net gain) or a ratchet improved beyond tolerance; else ledger outcome `no_improvement`
— NOT a rollback (never `rollbacks`), counts for `stalled` and `consecutive_no_improvements`. Gate
precedence: protected path > `red_file` > `skip_judges` > regression > improvement.
Do NOT name the SAME detector in `notify_if` and an entry's `until:`: the `until:` is resolved at the
start of the next session, so the human is paged before the recovery step runs.

## Writing a judge (a judge IS a goal)
A judge lives at `agg/judges/<name>.{sh,md}` and is referenced by its **bare NAME** in
`done_if`/`abort_if`/`notify_if`/`invariants`/an entry's `until:`.
- **`.sh`** = SCRIPT judge — runs and prints ONE line of verdict JSON to stdout. Runs from the PROJECT
  ROOT (sees the worker's files). `chmod +x` it.
- **`.md`** = LLM RUBRIC judge — agg sends it to the ruler as a READ-ONLY one-shot; it must return the same JSON.
- **Declared in `judges:` — a METRIC or SCORE judge over swe-metrics** (optional external tool):
  `judges: { cc_p95: { metric: { id: "complexity.cyclomatic", stat: "p95", tool: "lizard" }, max: 10 },
  quality: { score: { definition: "swe-default", version: "2" }, min: 80 } }`. No file. All such judges of a
  session share ONE `swe-metrics measure` (cache keyed on the tree, `metrics::measure`); a skipped/failed
  analyzer or absent metric ⇒ the judge ERRORS with the analyzer's reason (never met); several tools ⇒
  `tool:` or `combine: strictest|all|majority`; `scope: changed` ⇒ `--changed-since <base>`, and with
  no changed file its analyzer applies to the judge is VACUOUSLY met (`vacuous: true`, no value — moves
  nothing at the gate: `groups::assess` and `landed_met` skip it). A score is
  computed with the INCUMBENT's report (`agg/private/metrics/incumbent/`, replaced on every kept gate) as
  `--baseline`; definition/version mismatch refuses; `null` ⇒ error. Their ratchet may omit `direction`
  (`max:` ⇒ down, `min:` ⇒ up, score ⇒ up; checked against the report). Declared + a file of the same
  name, or no `swe-metrics` on PATH / wrong schema ⇒ startup refusal. Code: `src/metrics/`.
- **Evidence:** files named in a verdict's `evidence` (relative → `$AGG_JUDGE_SCRATCH` first, then the
  project) are COPIED after judging to `agg/private/evidence/<run_id>/<session>/<judge>/`
  (`core::evidence`, `features::evidence`), recorded on the ledger row (`evidence_saved`/
  `evidence_skipped`) and in the report. A path that resolves (symlinks included) outside the scratch
  and the project is REFUSED. Run-wide cap: `judge.evidence_max_mb` (default 100) — skip + record, never abort.

Verdict contract (to stdout):
```json
{"met": true, "value": 3, "max": 3, "target": 3, "rationale": "3/3 tests pass"}
```
Optional `"mixed": true` = the judge's own measurements moved in different directions — a ratchet does
not count that verdict as an improvement (`merge_requires_improvement`).
Optional `"vacuous": true` = met because there was nothing to measure — neither improvement nor
regression, and not a "was met" reading for the next gate.
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
agg/state/                   GITIGNORED runtime state THE WORKER WRITES (agg reads it as untrusted input)
  STATE.md                     worker-curated forward advice (rewritten wholesale each session)
  wiki/                        worker-owned OKF knowledge base — durable; MULTI-SESSION PLANS live here
  sessions/  spawns.json  spawns/  BLOCKED.md
agg/private/                 GITIGNORED runtime state AGG OWNS — carved OUT of the sandbox's writable set
  INSTRUCTIONS.md              the worker's whole `-p` (agg REGENERATES it every session; the worker's ORDERS)
  LOG.md                       durable institutional memory (worker's input arrives sanitized via sessions/)
  verdicts.jsonl  gate.json  calls.jsonl  clock.json  state.json  project.json  bus/  run.{pid,log}
  waits.jsonl                  every rate-limit wait: start (agent, until, partial ref) + how it ended
  report.json                  the end-of-run report — the STABLE interface for embedders (src/report/)
  violations.jsonl  protected/   write_deny: the rollback record + agg's copies to restore from
  evidence/<run>/<session>/<judge>/  the files each verdict cited, kept after its judge ran
  prompts.jsonl                per session: sha256 of every prompt_file/role_prompt_file its brief read
```
**The split rule:** if the worker writing it could change *when the loop ends, what it may spend, or what
agg believes happened*, it is private. `verdicts.jsonl` is why: forged `merged` rows make `stalled` report
met, so a project with `abort_if: "stalled"` has its worker end its own run. `src/paths.rs` is the
authority — the classification IS a test there. Binds only under `isolation: sandbox`/`container`; under
the default `none` the worker has the whole filesystem and no layout changes that.

**Protected paths (`write_deny`)** — `defaults.write_deny` ∪ each step's own; globs `*` `?` `**`,
project-relative. UNION ONLY: `writable:` never subtracts from it. Two layers, and the SECOND is the
guarantee: (1) the kernel denies the write under `sandbox` (Seatbelt regex on macOS; bwrap `--ro-bind`
on Linux) and `container` (`:ro` mounts); (2) on EVERY tier, `none` included, `features/protect.rs`
hashes every covered entry (tracked, untracked AND gitignored) right before the worker and compares the
moment it exits — a difference is restored before judging, recorded as `protected_path_violation` in
`agg/private/violations.jsonl`, and the gate rolls the span back regardless of verdicts or
`gate_regressions`. A bind tier (Linux sandbox, container) REFUSES any pattern but a literal path or
`dir/**` at startup; `none` warns that only the gate applies. Platform matrix: `docs/CONFIG.md`.
Code: `src/protect/` (glob, snapshot, sha256, the matrix), `src/features/protect.rs` (the bracket).

## The two ways to drive the loop (`src/` map)
One engine, two front ends. `agg.step()` and the YAML walk dispatch through the **same** primitive —
there is no second execution model, and adding one is the thing to refuse.

```
src/core/walk.rs      the WHOLE of the YAML flow (~30 lines): lap the list, honour times/until+max.
                      Exhausting `max` without the condition holding is an ERROR, not an advance.
src/driver/facade.rs  the Rust API — `Agg`, the eleven calls, lazy+memoized judges, `gate()`.
src/core/judge.rs     runs a judge (script/rubric/native) and confines it AS A JUDGE. The tier is the
                      STRONGEST any step declared — ⚠ NOT `cfg.run_isolation()`, which reads
                      `cfg.steps` and so returns `none` for every driver (they build steps in code).
                      Then: run-level tier,
                      project READ-ONLY, writes relocated to a shared per-session `$AGG_JUDGE_SCRATCH`.
src/core/verdicts.rs  `agg/private/verdicts.jsonl` — one row per judge per GATE, never per step.
src/metrics/          metric/score judges over swe-metrics: `declare` (startup: kinds, ratchet directions,
                      shared `--only` sets, the probe), `report`+`combine` (pure selection), `measure`
                      (ONE measurement per session tree — a process cache; the incumbent, promoted on a
                      kept gate and by the baseline pass), `score`. swe-metrics is OPTIONAL: nothing here
                      runs unless a judge is declared with `metric:`/`score:`.
src/core/ratchet.rs   the numeric ratchet + the gate's own history (rollbacks, oscillation). Engine
                      state (`eng.gate`), moved ONLY in `features::gate::keep_or_rollback` — the body
                      both front ends share — and persisted to `agg/private/gate.json` + driver
                      `Gate` call records (what `--resume` restores).
src/usage.rs          what every agent call REPORTED (model, 4 token kinds, cost) → `usage::Ledger` per session.
                      Charge through `LoopState::charge_usage`/`charge_ruler` ONLY — they feed the ceilings,
                      the per-agent tally and the report's `session_log` together. A kind an agent does not
                      report stays `None`; never fold one kind into another (cache writes are NOT output).
src/core/calls.rs     `agg/private/calls.jsonl` — the CALL ledger that makes `--resume` work (the driver's
                      calls; on the YAML path one `step` row per session + one `gate` row per judged one,
                      the step's label carrying the walk position — `features/resume.rs`).
                      Fast-forward is sound only back to the last KEPT gate (OD-12): everything after
                      it is parked on per-run span branches the ledger cannot carry.
src/assembly.rs       builds the run-set: `done_if ∪ abort_if ∪ invariants ∪ every until: ∪ ratchet keys
                      ∪ every group member` (a group named in `done_if` joins the DoD-set too).
                      A judge named by NO expression never runs, however many files sit in agg/judges/.
                      `build` COLLECTS every problem (a `Problem {path, message}`); `assemble` is
                      its startup face. `src/check.rs` (`agg check`) runs parse → build →
                      `capability::assess` → agent preflight with the same functions — add a new
                      startup refusal THERE (as a located `Problem`), never as a bare `bail!`, or
                      `agg check` stops seeing it.
src/schema.rs         `agg schema` = schemars over the config types. A new config struct needs
                      `JsonSchema` beside `Deserialize`; `tests/schema.rs` validates every shipped
                      example + the scaffold against it with an independent validator.
```
Driver-path traps: `Agg` boots **lazily** (a `cycles=0` driver publishes nothing and is invisible to
both readers); `eng.judges` is **empty** on that path, so anything reading it sees no judges — the
facade publishes its own into `dash.judges`. Tests: `tests/driver_api.rs`, `tests/samples.rs`.

## The three "agent" surfaces — do NOT conflate
- **The worker** (agg's inner agent) reads `agg/AGG.md` + the agg-composed `agg/private/INSTRUCTIONS.md`.
- **An agent OPERATING agg** uses the CLI above + the `/agg:*` skills (`agg:new` / `agg:status` / `agg:supervise`).
- **This file** is the condensed reference for an agent working in/with this repo.

## Working ON this repo (contributing to agg itself)
- **Gate before any commit:** `cargo build` (0 warnings) · `cargo test` · `cargo clippy --all-targets -- -D warnings` · `bash scripts/e2e.sh` (~12 min).
- **Run `cargo test` / e2e OUTSIDE the sandbox** (they SIGKILL processes + bind sockets; a sandboxed e2e gives false failures). If `rtk` rewrites cargo output, use `rtk proxy cargo …`.
- **THE MOAT — never break it:** gitignore ONLY `agg/state/` + `agg/private/`, NEVER all of `agg/`. The judges must stay committed so a rollback can restore a grader a worker tampered with.
  The same entries go to `.git/info/exclude` (`git::ensure_agg_excluded`): a rollback can delete an
  uncommitted-on-base `.gitignore`, never `info/exclude`.
- **A session cut short (rate limit, Ctrl-C, `agg stop` mid-session) is INCOMPLETE — never judged, never
  lost:** its work goes to `refs/agg/partial/<run>/<session>` and the tree is reset (`features/partial.rs`,
  `git/partial.rs`). A rate-limit wait publishes `waiting: {reason, agent, until}` in `state.json`
  and the report, and its seconds are excluded from `work_time`.
- **agg owns ALL git; the worker NEVER runs git** — the worker just edits files. agg auto-commits its work on the session branch (a `GitAutoCommit` handler on `on_verify`), then merges/keeps or rolls back. The worker never adds/commits/merges/pushes. (GIT_REDESIGN.)
- **Verify agent behavior on the WIRE** (`scripts/e2e_real.sh`), never from docs — agent CLIs differ (Codex's `-p` is `--profile`; only Claude reports dollar cost; etc.).
- **Design specs live in `internal/`** (gitignored): `SEQUENCES.md`, `STATE_REDESIGN.md`.
