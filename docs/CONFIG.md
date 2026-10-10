# `agg` configuration reference

One file, `agg/agg.yaml`, holds everything: `defaults` / `judge` / `steps` / `sequence` plus a few
top-level survivors. A judge IS a goal, resolved by name from disk
(see [Judges](#judges-resolved-by-name)). Every struct is parsed with `deny_unknown_fields`, so a
misspelled or misplaced key is a **hard error at startup**, never a silent no-op. (That guard is what
makes a stray top-level `budget:` — the three ceilings now live unified under `sequence.limits:` — fail
loudly instead of becoming a decorative spend ceiling.)

The only required key is `project`. Everything else has a default (a file with no `sequence:` is a
Rust-driver project — see [`docs/RUST_API.md`](RUST_API.md)).

### Validating a config without running it — `agg check` · `agg schema`

- **`agg check [--json]`** runs EVERY startup check `agg run` makes — parse, step and judge
  resolution, the condition grammar, what the agents can do, the isolation tier's availability, each
  agent CLI on PATH — and lists **all** problems at once instead of stopping at the first. It runs no
  judge and no agent. Each problem carries a dotted path into this file (`steps.worker.isolation`,
  `sequence.steps[1].until`; `""` = the file as a whole) and the message startup would print. Exit
  **0** valid · **1** any problem. `--json`:
  ```json
  {"schema_version": 1, "ok": false, "config": "agg/agg.yaml",
   "problems": [{"path": "defaults.agnet", "message": "unknown field `agnet`, expected one of `agent`, …"},
                {"path": "sequence.done_if", "message": "no judge named `ghost` …"}],
   "warnings": []}
  ```
  `warnings` are non-fatal (a dollar guard inert on an agent that caps itself).
- **`agg schema`** prints the JSON Schema (draft 2020-12) of this file. It is generated from the very
  types agg parses into, so it cannot drift: every `deny_unknown_fields` struct is
  `additionalProperties: false`, and `$comment` names the agg version that produced it. Point an
  editor's YAML language server at it, or validate a generated config before handing it to agg.
  The schema checks structure and types; `agg check` additionally checks what only agg can know
  (judge files exist, the agents and tiers are available on this host).

```yaml
project: "my-project"

# Inherited by EVERY step; a step body may override any of these.
defaults:
  agent: "claude"                  # the WORKER default: claude · codex · copilot
  model: "claude-opus-4-8[1m]"     # None ⇒ the agent's own default (codex: OMIT; copilot: auto)
  effort: "high"                   # low|medium|high|xhigh|max — None ⇒ backend default; "" ⇒ none
  worker_args: []                  # extra flags passed VERBATIM to the worker (the sandbox constraint)
  state: "state/STATE.md"          # the forward-advice file under agg/ (gitignored)
  isolation: none                  # none (default) | sandbox | container — the blast-radius jail
  image: "alpine:3.20"             # the base image an `isolation: container` step runs in
  write_deny: ["tests/oracles/**"] # PROTECTED paths: readable, never changed — on EVERY tier (below)

# THE RULER — runs LLM judges + the summarizer. Run-level and IMMUTABLE: naming any of these keys
# in a step body is a hard error (a grader that moves makes verdicts incomparable across cycles).
judge:
  agent: "claude"
  model: "claude-haiku-4-5-20251001"   # None ⇒ the ruler's cheap default. A cheap model grades.
  timeout: 300                         # seconds — EVERY judge, script and LLM alike

# The step palette. NAME → a body of OVERRIDES over `defaults:`. The name is your own label.
steps:
  worker: {}                       # pure defaults
  reconsider:                      # (example) a stall-triggered step-back on a different vendor
    agent: "codex"
    role_prompt: "Step back — assume the current approach is wrong."   # ROLE framing above prompt:
    prompt: "Name 2-3 different approaches, pick one, record the rejected ones + why."
    skip_judges: true

# The repeating sequence + the run-level ceilings and Definition of Done.
sequence:
  steps:
    - { step: worker, until: "all_tests_pass", max: 4 }   # hammer, but stop as soon as it lands …
    - reconsider                   # … and when 4 sessions did not land it, step back once
  limits:                          # the run-level ceilings, unified. Each null/absent = unlimited.
    tokens: 5000000                # output-token ceiling (worker AND judge spend) → over_budget
    cost: null                     # dollar ceiling → over_cost. CLAUDE-ONLY (null = unlimited)
    sessions: null                 # session cap → over_iterations (null = unlimited)
  gate_regressions: true           # roll a session back if a previously-met judge regresses
  invariants: ["no_regression"]    # judge names that must STAY met
  ratchet:                         # NUMERIC no-regression: a value may not get worse than at the
    cc_p95:   { direction: "down", tolerance: 0.5 }              # last kept gate (± tolerance)
    mutation: { direction: "up", tolerance: 1.0, baseline: 71.0 } # seeded reference for session 1
  groups:                          # named judge lists the aggregates range over: count_met(static_quality) >= 2
    static_quality: ["sonar", "codescene", "lizard"]
  merge_requires_improvement: false  # true ⇒ merge a session only if a goal or a ratchet IMPROVED
  done_if: "correct_result AND all_tests_pass AND coverage.value >= 80"
  abort_if: "over_budget OR work_time >= 28800 OR any_regressed(invariants) OR any_judge_error OR consecutive_rollbacks >= 3 OR oscillating"
  notify_if: "stuck.value >= 85"   # NON-TERMINAL twin of abort_if: ping a human, KEEP RUNNING.
                                   # STRICTER than whatever bounds the repeat above, deliberately —
                                   # see "the escalation ladder". (`stalled` ships; `stuck` you author.)
  notify:                          # how that ping is delivered (also fires once on an abort_if halt)
    cmd: ["curl -s --max-time 10 -d {{reason}} ntfy.sh/my-topic"]   # FOREGROUND — always bound it
    cooldown_sessions: 3           # debounce: min sessions between two notify_if fires (0 = every cycle)

# ---- top-level survivors (all optional) ----
heartbeat_secs: 30
watchdog: { idle_secs: 900, cpu_grace: 180 }
ratelimit_backoff_secs: 1800       # claude + codex; copilot cannot flag a rate-limit. Used when no reset time is reported
ratelimit_min_gap_secs: 60         # least time between a rate-limited attempt and the next (`agg send wake` waits it out)
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
| `state` | `state/STATE.md` | the forward-advice file (see [State](#state-and-memory)) — under `agg/state/` (gitignored), resolved against `agg/`. |
| `role_prompt` | none | generic **role** framing composed *above* a step's `prompt:`. Inheritable; a step body may override it. |
| `role_prompt_file` | none | `role_prompt` read from a **file** ([Prompts from files](#prompts-from-files--prompt_file-role_prompt_file)). An alternative to `role_prompt` — set one. |
| `isolation` | `none` | `none` \| `sandbox` \| `container` — blast-radius jail (below). Inheritable; a step may override. |
| `image` | `alpine:3.20` | the base image an `isolation: container` step runs in. Inert on every other tier. |
| `write_deny` | `[]` | **protected paths** — globs every step may read but never change. Enforced on **every** tier by the gate's content-hash check, and by the kernel under `sandbox`/`container` ([below](#protected-paths--write_deny-readonly-writable)). A step's own list is ADDED to this one. |

## `judge` — THE RULER

The one agent + model that runs every **LLM** judge and the summarizer. It is a separate, run-level
block precisely because it is immutable: a moving grader makes verdicts incomparable across cycles.
**Naming `agent` / `model` / `timeout` in a step body is a hard error.**

| key | default | notes |
|---|---|---|
| `agent` | `claude` | usually the same as `defaults.agent`, but need not be — a Codex worker with a Claude ruler is valid. |
| `model` | none | none ⇒ the ruler's cheap-model default. |
| `timeout` | `300` | seconds, enforced for **every** judge (script and LLM) with a process-group kill. |
| `evidence_max_mb` | `100` | the most judge evidence (MiB) one run copies into `agg/private/evidence/` ([below](#evidence--what-agg-keeps)). A disk bound, not a run limit: a file that does not fit is skipped and recorded, and the run goes on. |

If the ruler is rate-limited or unreachable, the run **parks in backoff and merges nothing** — it
never fails over to another agent (that would move the ruler) and never fabricates a verdict.

## `judges` — per-judge overrides

Plural `judges:` is not the ruler. It is a map of judge NAME → overrides, with **exactly one key**:

| key | default | notes |
|---|---|---|
| `timeout` | `judge.timeout` (300) | seconds, for this judge alone. |

```yaml
judges:
  load_ok:   { timeout: 2700 }   # 45 minutes of synthetic load
  e2e:       { timeout: 1200 }
```

Only judges that need it appear here; everything else uses the run-level default and is never
listed. This is the one per-judge knob that survived the judge-level `gate:`/`when:` cut, and it
survived because it is not flow and not gating: a judge slower than 300s simply **dies** without it.

⚠ It does not make an expensive judge cheap — in YAML every run-set judge runs after every judged
step, so a 45-minute judge costs 45 minutes per step. Short-circuiting an expensive judge behind
cheap ones is a Rust-path feature (`agg.judge(&tests).met() && agg.judge(&load).met()`); see
`examples/workflow.yaml`, which spells out that trade in full.

## `steps` — the palette

Each NAME maps to a body of overrides. **The complete legal key list** (anything else is a hard error):

| key | default | notes |
|---|---|---|
| `agent` | `defaults.agent` | per-step agent — a different **vendor** is the strongest perspective diversity. |
| `model` | `defaults.model` | per-step model — grunt work on a cheap model, the step-back on a strong one. |
| `effort` | `defaults.effort` | validated against the step's backend. |
| `worker_args` | `defaults.worker_args` | |
| `state` | `defaults.state` | |
| `role_prompt` | `defaults.role_prompt` | generic **role** framing composed **above** the step's `prompt:` (e.g. `reconsider`'s "step back — assume the current approach is wrong"). Replaced the old hardcoded `Role` enum. |
| `role_prompt_file` | `defaults.role_prompt_file` | `role_prompt` from a file. The role pair inherits **as a pair**: a step naming either key replaces both of the defaults'. |
| `prompt` | none | **ADDITIVE** to the composed prompt, never replacing it. |
| `prompt_file` | none | `prompt` read from a **file** (below). An alternative to `prompt` — set one. |
| `skip_judges` | `false` | `true` ⇒ no judges run after this step, so nothing merges — the work **stages** (below). |
| `isolation` | `defaults.isolation` | `none` \| `sandbox` \| `container` — the blast-radius jail for this step (below). |
| `image` | `defaults.image` | the base image for this step under `isolation: container`. |
| `readonly` | `[]` | project paths this step may read but not write — **kernel only**: inert under `isolation: none` (agg warns). No `defaults.readonly`. ([below](#protected-paths--write_deny-readonly-writable)) |
| `writable` | `[]` | paths subtracted from this step's `readonly` (exact match). Never touches `write_deny`. |
| `write_deny` | `[]` | MORE protected paths for this step, **unioned** with `defaults.write_deny`. Nothing subtracts from it. |

### Prompts from files — `prompt_file`, `role_prompt_file`

A prompt kept in its own file is reviewed and diffed as a document, and can be shared with other
tooling, instead of living as a YAML string:

```yaml
defaults:
  role_prompt_file: "roles/worker.md"          # relative to agg/ — or an absolute path
steps:
  implement: { prompt_file: "steps/implement.md" }
  review:    { role_prompt_file: "roles/reviewer.md", prompt: "Review the last change." }
```

- **One form per key.** Naming `prompt` and `prompt_file` (or `role_prompt` and `role_prompt_file`)
  in the same block is a startup error, located at the `…_file` key, and so is a file that cannot be
  read. `agg check` lists both.
- **Read at every session start.** The text is read when the session's brief is composed, so an edit
  applies from the next session — no restart. A file that has gone missing mid-run ends the run with
  an `error`: a brief without its task would send the worker off on the wrong work.
- **Hashed per session.** Each file read is recorded in `agg/private/prompts.jsonl` — one row per
  session: `{session, step, files: [{key, path, sha256}], ts}` — so a run shows which version of a
  prompt each session was given. Inline prompts are in `agg.yaml` itself and record nothing.
- The brief is **the same** as with the inline keys: the file's text is used exactly as an inline
  string would be.

A Rust driver has the same for a step's task: `Step::new("implement").prompt_file("steps/implement.md")`.

**`skip_judges` steps stage.** Nothing was judged, so nothing merges; the work stays on the session
branch and the **next judged step gates the whole span** — pass ⇒ the span merges, a regression ⇒ the
whole span rolls back. A sequence of *only* `skip_judges` steps is refused at startup (nothing could
ever merge).

### `isolation` — blast-radius jail (a DIFFERENT axis from session isolation)

Every worker runs the agent in auto mode (`--dangerously-skip-permissions` and equivalents), so by
default it can do anything YOU can — `rm -rf ~`, read `~/.ssh`, touch other repos. `isolation` bounds
that, **per step**:

| tier | what the worker may do |
|---|---|
| `none` *(default)* | no confinement — full host access, today's behaviour. |
| `sandbox` | **write** = the project dir (+subfolders) **minus `agg/private/`** + `$TMPDIR` + the agent's own state dir; **read** = everything; **network** = fully open. Kernel-enforced. Also confines this step's judges + hooks. |
| `container` | the worker runs **inside a container** (`docker run` / `podman run`) with the project dir bind-mounted: **write** = the project dir **minus `agg/private/`** + the agent's state dir + the container's own tmp; **read** = only what is mounted; **network** = fully open. |

`sandbox` uses the OS jail — **`sandbox-exec` (Seatbelt) on macOS**, `bwrap` (bubblewrap) on Linux —
except Codex, which has its own kernel sandbox agg drives with flags. It confines the **worker, its
script + LLM judges, and its foreground hooks** in one jail, so nothing a confined worker can rewrite
in the project dir becomes an escape. It does **not** restrict network (full internet by design).

**The one hole punched in "the project dir is writable" is `agg/private/`** — agg's own runtime state
(the verdict ledger, the command bus, the pidfile, the worker's brief). It stays fully **readable**; only
writes are denied, on both confining tiers. Without that carve-out a confined worker could append forged
rows to `agg/private/verdicts.jsonl` and make `stalled` report met — ending its own run under
`abort_if: "stalled"`. See [State and memory](#state-and-memory) for what lives on each side and why.

`container` confines from the other side: instead of jailing a host process it re-hosts the command
in a container, so the host filesystem is not there to read at all. The step's `image:` names the
base image (default `alpine:3.20`). It applies to **every** agent, self-sandboxing or not — the
container boundary *is* the confinement.

> **`container` is the confinement mechanism, not yet an agent runtime.** The base image has no
> agent CLI and no credentials in it, so this tier is ready for commands you can run in a plain
> image; running `claude`/`codex` themselves inside it needs an image with the CLI plus a mounted
> credential store, and is a documented follow-up (`internal/ISOLATION.md` §15). That section also
> records the residual: unlike `sandbox`, this tier confines the **worker only** — judges and hooks
> are host tooling (they run `cargo`, `git`, your linters), so they still run on the host.

If `sandbox` is requested but the OS mechanism is unavailable — or `container` is requested and no
container engine answers — agg **refuses at startup**, never a silent downgrade to `none`. `agg
doctor` reports whether the tooling the config asks for is present.

> **Platform status:** macOS (`sandbox-exec`) is verified end-to-end. The Linux `bwrap` path is
> implemented but not yet shaken out on a real Linux host — treat Linux `sandbox` as experimental.

This is **orthogonal** to `session_isolation` (below): that protects the repo *history* from bad work
(per-session branches + a rollback gate); `isolation` protects the *host* from an errant worker. They
compose.

### Protected paths — `write_deny`, `readonly`, `writable`

An agent that cannot pass a test will, given the chance, edit the test: delete it, special-case it,
or weaken the golden file it compares against. Committed judges are restored by a rollback, but an
oracle **outside** `agg/judges/` that the worker weakens makes the judges pass — and the session
merges. Three keys say which project paths the worker may read but not change:

| key | where | what binds it | under `isolation: none` |
|---|---|---|---|
| `write_deny` | `defaults` + each step (**union**) | the **kernel** (confining tiers) **and the gate check** (every tier) | **enforced** — the gate rolls the session back |
| `readonly` | each step | the kernel only | inert — agg warns that the step *can write* them |
| `writable` | each step | subtracts from that step's `readonly` (exact spelling) | — |

**Use `write_deny` for anything a judge trusts** — oracles, golden masters, contract tests, rule
files, gate configuration, reference material. `readonly`/`writable` are the older, kernel-only lists,
useful for paths you would like a step not to touch but that no judge depends on.

```yaml
defaults:
  isolation: "sandbox"
  write_deny: ["tests/oracles/**", "rules/", ".gitlab-ci.yml"]
steps:
  implement: {}
  refactor: { write_deny: ["src/api/**"] }   # refactor may not touch the API either — ADDED, never instead
```

**Patterns** are relative to the project root, `/`-separated: `*` and `?` match inside one path
component, `**` matches zero or more whole components. `dir/` is shorthand for `dir/**`, and
`dir/**` covers the directory **itself** too, so swapping it for a symlink or deleting and recreating
it counts as a change. Refused at startup, on every tier: an absolute path, `..`, a pattern naming the
project root, and syntax agg does not implement (`[…]`, `{…}`, a leading `!`, `\`) — read literally,
those would protect nothing while looking like they did. Matching is on **canonical** paths (a file
reached through a symlinked directory is matched where it really lives) and **case-insensitive** on a
case-insensitive volume (the macOS default), where `TESTS/X.JSON` *is* `tests/x.json`.

**Union, never subtraction.** A step's `write_deny` is added to `defaults.write_deny`; `writable:` does
not reach it. A step can only ever be *more* restricted than the project default, which is what lets
an operator rely on the default list. The Rust driver's twin is `Step::write_deny([..])`, which
accumulates down a template chain the same way.

#### Two layers — and the gate check is the guarantee

1. **The kernel (first line).** Under `sandbox`/`container` the patterns go to the same mechanism as
   `agg/private/`: the write is refused (`EPERM` / `EROFS`) before it happens. Reads stay open, so
   judges and the worker still read every protected file.
2. **The gate check (the guarantee, every tier).** Right before the worker starts, agg fingerprints
   every covered entry — **tracked, untracked and gitignored** (a `git diff` would miss the last two):
   a file's bytes (SHA-256) and permission bits, a directory's permission bits, a symlink's target.
   The moment the worker exits it does it again. Any difference — a path changed, appeared or
   disappeared — is a **`protected_path_violation`**:
   - the paths are **restored** from agg's own copies (`agg/private/protected/<sha256>`, each checked
     against the hash agg holds in memory) **before any judge runs**, so nothing is ever graded
     against a weakened oracle;
   - a row is appended to **`agg/private/violations.jsonl`**:
     `{"session":3,"step":"implement","reason":"protected_path_violation","window":"session","paths":["tests/oracles/x.json"],"restored":true,"ts":…}`;
   - the gate **rolls the session back** — the whole span, if it was staged — whatever the judges
     said, and whatever `gate_regressions` (or a driver's `on_regression`) is set to.

   A change made **after** a session's check — by a process the worker left running, say, or a hook —
   is caught when the next session starts: restored, and recorded with `"window":"between_sessions"`
   against the session before. A restore that fails is a hard stop of the run, never a silent pass.

#### Platform matrix

| tier | platform | `write_deny` is… | how |
|---|---|---|---|
| `none` | any | **gate check only** — agg warns at startup | snapshot + compare + restore + rollback |
| `sandbox` | macOS | **kernel-enforced** + gate check | one Seatbelt `(deny file-write* (regex …))` per pattern — any glob — case-folded on a case-insensitive volume, plus an unlink deny on each ancestor of the pattern's literal directory |
| `sandbox` | Linux | **kernel-enforced** for a literal path or `dir/**`; any other glob is **refused** at startup; + gate check | `bwrap --ro-bind` of the directory or file (a `dir/**` root that does not exist yet is created first; a literal path that does not exist yet has no mount point and is left to the gate, with a warning) |
| `container` | any host | same as Linux `sandbox` | a `:ro` bind mount per directory or file |

The refusal is the rule "refuse rather than degrade": a bind can only cover a path that exists when
the session starts, so a pattern like `**/*.golden` — which names files that do not exist yet, in
directories it does not list — cannot be kernel-enforced there. Protect the directories that hold
them (`fixtures/**`) or list literal paths.

#### What each bypass meets (measured)

| attempt | `none` | macOS `sandbox` | `container` (Docker) |
|---|---|---|---|
| direct write · child-process write · `chmod` | caught | blocked | blocked |
| delete, or delete and recreate | caught | blocked | blocked |
| rename another file over it | caught | blocked | blocked |
| symlink to it, write through the link | caught | blocked | blocked |
| hard link to it, write through the link | caught | blocked (the link itself is refused) | blocked (`EXDEV`) |
| `git checkout`/`git restore` an old version | caught | blocked | blocked |
| new file inside a protected directory | caught | blocked | blocked |
| change a **gitignored** file inside one | caught | blocked | blocked |
| case variant (`TESTS/X.JSON`) on a case-insensitive volume | caught | blocked | blocked |
| move an **ancestor** away (`mv tests tests_old`) | caught | blocked | **caught** — the mount moves with it |
| swap the protected directory for another | caught | blocked | caught |
| write through a hard link that existed **before** the session | caught | **caught** — no path rule names the link | caught |

*caught* = the gate restored the path and rolled the session back; *blocked* = the kernel refused the
write and the rest of the session's work is judged normally. Linux `sandbox` (bwrap) uses the same
bind mechanism as the container column and is expected to behave like it; it is not yet verified on a
real Linux host. The suite that produced this table is `tests/protected_paths.rs`.

**What is NOT covered.** `.git/` and `agg/private/` are never part of the check — agg owns both and
writes them during a session. Judges and hooks are operator code: a change they make is caught at the
next session start (above), not attributed to the session that ran them.

## `sequence` — the loop

### `sequence.steps` — the entries

A list of entries. Each is a **step name**, or a mapping that says how many times to dispatch it:

```yaml
steps:
  - survey                                    # run `survey` once
  - { step: fix, times: 4 }                   # run `fix` 4 times, then move on
  - { step: build, until: tests_pass, max: 8 } # repeat `build` until the condition holds, ≤ 8 times
```

| key | meaning |
|---|---|
| `step` | the step to dispatch — a key in `steps:`. A bare `- survey` is shorthand for `{ step: survey }`. |
| `times` | dispatch it exactly this many times (≥ 1) before moving on. |
| `until` | repeat until this expression holds — the [condition grammar](#the-condition-grammar) below, re-evaluated **after** each dispatch. Requires `max`. |
| `max` | the ceiling on an `until` repetition (≥ 1). |

- `times` and `until`+`max` are **alternatives** — a fixed count and a condition cannot both bound
  one entry, and a bound is mandatory on both. An unbounded repetition in a config file is a run you
  cannot reason about; `limits:` is a RUN-level ceiling, not a per-entry one.
- `until` is checked only **after** a dispatch (`repeat … until`), so a condition that is already
  true still buys exactly one session.
- There is **no `if:`** — a lap dispatches every entry, in order. Conditional flow belongs to the
  Rust driver API, not to this file. Naming `if:` on an entry is a parse error, not a silently
  ignored key.
- An **unknown step name, or a judge name that resolves to no file, is a hard error at startup**,
  listing what does exist. Never a runtime surprise. So is a typo'd entry key, by name.

The sequence repeats from the top, forever, until `done_if` fires (exit **0**) or `abort_if` fires
(exit **3**). When both hold at the end of the same session, **`done_if` wins**; an operator stop
(`agg stop`, SIGTERM, SIGINT) that arrived during that session beats both. Every ending writes
[`agg/private/report.json`](#the-run-report-aggprivatereportjson). Before session 1, every judge in the run-set runs once against the untouched repo (the
**baseline**), so a run can end immediately as already-done or already-aborting.

A judge named in an `until` condition joins the **run-set** (it has to execute, or the condition
could never become true) but never the **DoD-set** — it is machinery, not a goal.

### `sequence` keys

| key | default | notes |
|---|---|---|
| `limits: { tokens, cost, sessions }` | all unlimited | The run-level ceilings, unified. Each key `null`/absent = unlimited. The three subkeys below. **No ceiling is implied:** with no `limits` at all a run ends only on `done_if`, `abort_if` or a stop. |
| `limits.tokens` | unlimited | **output-token** ceiling → `over_budget`. Counts **worker AND judge** spend, summed across all agents. Works on every agent. Output only: cache writes are input and are not counted (agg ≤ 0.0.15 added Claude's cache writes to output, so the same work now counts far fewer tokens). |
| `limits.cost` | unlimited | **dollar** ceiling → `over_cost`. **CLAUDE-ONLY** — see [Choosing an agent](#agent-specific-rules). |
| `limits.sessions` | unlimited (`null`) | session cap → `over_iterations`. A **non-zero** `agg run --max-sessions <n>` overrides it; the flag's default `0` falls back to this key (not to unlimited). |
| `limits.wall_time` | unlimited | **END-TO-END wall ceiling in SECONDS**, human waiting INCLUDED — a *deadline*. ⚠ Replaces `wall_hours`; the unit changed by 3600×, so the old key is a **hard error at startup**, never an alias. |
| `limits.work_time` | unlimited | **EFFORT ceiling in SECONDS**: `wall_time` minus time spent blocked on a human. The ceiling a run with humans in it actually wants — see [the clock](#the-clock--three-terms-all-in-seconds). |
| `gate_regressions` | `true` | roll a session back if a previously-met judge now fails. The rename of the old `rollback_on_regression`. |
| `invariants` | `[]` | judge names that must STAY met. The gate protects them; `any_regressed(invariants)` gives up on them. |
| `ratchet: { <judge>: { direction, tolerance, tolerance_pct, baseline } }` | none | the **numeric** no-regression rule — see [The ratchet](#the-ratchet--numeric-no-regression). `direction` (`up` = higher is better, `down` = lower is better) is required; `tolerance` (absolute, default `0`), `tolerance_pct` (% of the reference; the larger allowance wins) and `baseline` (the seeded reference) are optional. A ratcheted judge joins the run-set, never the DoD-set. |
| `groups: { <name>: [<judge>, …] }` | none | named judge lists — see [Judge groups](#judge-groups--k-of-n). Every aggregate takes a group as a subset: `count_met(quality) >= 2` is "two of three". Every member joins the run-set; a group named in `done_if` puts its members in the DoD-set. Refused at startup: an unknown member, an empty group, a member listed twice, a name the grammar reserves. |
| `merge_requires_improvement` | `false` | merge a judged session only if something **improved** — see [Merge only on improvement](#merge-only-on-improvement--merge_requires_improvement). A session that improved nothing is not merged and is recorded `no_improvement`: not a rollback, but it counts for `stalled`. |
| `oscillation_window` | `4` | how many earlier judged sessions [`oscillating`](#rollbacks-and-oscillation--the-gates-own-detectors) looks back over for an A→B→A repeat. Must be `>= 2`. |
| `done_if` | `all_goals` | the **Definition of Done** — success stop (exit 0). |
| `abort_if` | none | the giving-up guard (exit 3). |
| `notify_if` | none | the **non-terminal twin of `abort_if`**: same grammar, but true ⇒ run `notify.cmd` and **keep looping**. Requires a non-empty `notify.cmd`. See [Stuck detection and notification](#stuck-detection-and-notification). |
| `notify: { cmd, cooldown_sessions }` | none | how a notification is delivered. Legal **without** `notify_if` — that is the "ping me when `abort_if` stops the run" policy. |

## The condition grammar

`done_if`, `abort_if`, and every `if` condition use one whitelisted boolean grammar (there is no
second expression language). Operators: `AND` / `OR` / `NOT` (word or symbolic `&& || !`), the
comparisons `== != >= <= > <`, and parentheses. Precedence: `or > and > cmp > atom`.

**Terms:**

| kind | terms |
|---|---|
| **judge (bare name)** | any judge name → its `met` **bool**. |
| **judge accessor** | `name.value`, `name.max` → the number the judge emitted. (`.target` is NOT an accessor — it is presentational.) `name.ref` → a **ratcheted** judge's current reference ([the ratchet](#the-ratchet--numeric-no-regression)); on any other judge it is a startup error. |
| **aggregates** | `all_goals`, `count_met`, `count_regressed`, `total`, `met_fraction`, `any_regressed` — bare, over the DoD-set; with a subset argument, over that subset. |
| **run scalars/ceilings** | `tokens_spent`, `budget_total`, `over_budget`, `cost_spent`, `cost_limit`, `over_cost`, `iterations`, `max_iterations`, `over_iterations`, `wall_time`, `human_wait_time`, `work_time`, `any_judge_error` |
| **gate history** | `rollbacks`, `consecutive_rollbacks`, `no_improvements`, `consecutive_no_improvements` (numbers), `oscillating`, `ratchet_regressed` (bools) — see [the gate's own detectors](#rollbacks-and-oscillation--the-gates-own-detectors). Process-internal, so they may sit in `abort_if`. |
| **subsets** | `(invariants)` or `(<group>)` — an argument on exactly the six aggregates, e.g. `any_regressed(invariants)`, `count_met(quality) >= 2`, `all_goals(quality)`. A [group](#judge-groups--k-of-n) is a name from `sequence.groups`; an unknown one is a startup error listing the defined groups. |

### Numeric thresholds — use the accessor

```yaml
done_if: "tests_pass AND coverage.value >= 80"   # ✅  read the number the coverage judge emitted
done_if: "tests_pass AND coverage >= 80"         # ✗  HARD ERROR — a judge name is a BOOL
```

A bare judge name is its `met` boolean; comparing a bool to a number is meaningless, so agg **refuses
it at startup** and tells you to use `coverage.value >= 80`. A threshold has one owner — the condition
— so the judge's own `target` is presentational only (it drives progress bars, nothing more).

### The two quantifiers — the DoD-set vs the run-set

- **The RUN-SET** = every judge named in `done_if` ∪ `abort_if` ∪ `notify_if` ∪ `invariants:` ∪ every
  entry's `until:` ∪ every `ratchet:` key ∪ every member of every `groups:` entry. These are the judges that actually execute after each step. `any_judge_error` ranges
  over this set. (This is why a `stalled` judge used only in an `until:` — or a `stuck`
  detector used only in `notify_if` — runs, without being listed anywhere else.)
- **The DoD-set** = judges named in `done_if` (bare, or as members of a group `done_if` uses) ∪
  `invariants:` only. The bare aggregates (`all_goals`, `count_met`, `total`, `met_fraction`,
  `any_regressed`) range over **this** set — and it is what the scoreboard's `N/M` counts.

They differ deliberately: if `all_goals` ranged over the run-set, `done_if: all_goals` could not be
true until `stalled` was met — i.e. the loop would "succeed" by getting stuck. **Never put a judge
that only bounds an `until:` into `done_if`.** A `notify_if` detector is machinery, not a goal,
for exactly the same reason: it is run-set only, so it never counts toward `N/M` and a session is
never rolled back because a detector flipped.

### `abort_if` is not part of the DoD

Done is one thing; giving up is another. `abort_if` is a ceiling (budget, time, a regressed
invariant, a judge error) that exits **3** so automation can tell a guardrail bail from a real win.
Typical values:

```yaml
# any agent:
abort_if: "any_regressed(invariants) OR over_budget OR over_iterations OR work_time >= 28800"
# claude only — add the dollar ceiling:
abort_if: "any_regressed(invariants) OR over_cost OR over_budget OR over_iterations OR work_time >= 28800"
```

Do **not** fold a ceiling into `done_if` — putting `over_budget` there would report a blown budget as
*success*. **Never leave an autonomous loop with no ceiling at all.** `any_judge_error` is `true` when
a judge that ran this step crashed / timed out / emitted garbage — an `error` is never a regression
and never satisfies `done_if`; wiring `abort_if: … OR any_judge_error` is the explicit policy.

## The ratchet — numeric no-regression

`invariants` protect a **boolean**: a judge that was met may not become unmet. A ratchet protects a
**number**: a judge's `value` may not get worse than it was at the **last kept gate**, by more than its
tolerance. Use it for the metrics a worker would otherwise trade against each other — complexity,
duplication, mutation score, latency.

```yaml
sequence:
  ratchet:
    cc_p95:   { direction: "down", tolerance: 0.5 }                # lower is better
    latency:  { direction: "down", tolerance_pct: 5 }              # noisy: 5% of the reference
    mutation: { direction: "up", tolerance: 1.0, baseline: 71.0 }  # higher is better, seeded
  abort_if: "consecutive_rollbacks >= 3 OR cc_p95.ref > 20"
```

- **The reference** is the judge's value at the last kept gate. Before the first keep it is the
  `baseline:` seed, else the value the pre-session [baseline pass](#session-isolation-mandatory-and-the-gate)
  measured on the untouched repo. With both, the **seed wins** and a gap beyond tolerance is printed as a
  *measurement drift* warning. A rolled-back value never becomes the reference; an improvement that is
  kept always does.
- **The rule.** A session whose value moved the wrong way by more than `max(tolerance, |ref| ×
  tolerance_pct / 100)` is a **gate regression**: under `gate_regressions: true` it is rolled back. The
  operator sees ``ratchet `cc_p95`: 10.4 → 11.5``, and the judge's row in `verdicts.jsonl` carries
  `"ratchet": {"from": 10.4, "to": 11.5}`. With `gate_regressions: false` the session is kept and the
  reference follows it — "last kept" is the rule either way.
- **An unmeasured value never passes.** A ratcheted judge that errors (or emits no number) on a session
  is a ratchet regression (`"to": null`), and `any_judge_error` is true. That is stricter than the
  boolean rule, where a flake keeps the merge — deliberately: a ratchet is a *no worse than* promise,
  and a value nobody measured cannot keep it.
- **Refused at startup:** a ratcheted judge that emits no `value` on the baseline pass (there is nothing
  to ratchet), one that **errors** on the baseline pass with no `baseline:` seed (nothing to compare
  session 1 against), a ratchet naming no judge file, a negative or non-finite tolerance, a missing
  `direction`, an unknown key.
- **Run-set, not DoD-set.** A ratcheted judge executes every judged step (it has to, to be compared),
  but it is a gate rule, not a goal: it never counts toward `all_goals` or the `N/M` scoreboard. Name
  it in `done_if` too if you also want its `met` as a goal.
- **Terms:** `ratchet_regressed` is true when the most recent judged gate recorded a ratchet regression;
  `<judge>.ref` is a ratcheted judge's current reference (no reference yet ⇒ every comparison is false).

The ratchet's state lives in agg's memory and is persisted to `agg/private/gate.json` (and, on the Rust
driver path, on every `gate` record of the call ledger, which is what a `--resume` restores it from).
It is never read back from `verdicts.jsonl`, so the worker cannot move its own reference.

## Judge groups — k of n

Several judges can measure one thing — three static-analysis tools, two security scanners — and they
disagree. A **group** names them, so a condition can count over just them:

```yaml
sequence:
  groups:
    static_quality: ["sonar", "codescene", "lizard"]
    security:       ["semgrep", "trivy"]
  done_if: "tests_pass AND count_met(static_quality) >= 2 AND all_goals(security)"
  abort_if: "any_regressed(security) OR over_budget"
```

- **Every aggregate takes a group**, the way it takes `(invariants)`: `count_met(g)`, `total(g)`,
  `met_fraction(g)`, `all_goals(g)`, `any_regressed(g)`, `count_regressed(g)`. All of them is
  `all_goals(g)`; any of them is `count_met(g) >= 1`; *k of n* is `count_met(g) >= k`. **The group carries
  no threshold** — a threshold has one owner, the condition.
- **A group is its members.** Every member joins the run-set, named by a condition or not. A group named
  in `done_if` puts its members in the DoD-set (so they count toward `all_goals`, the `N/M` scoreboard and
  the gate); one named only in `abort_if`, `notify_if` or an `until:` leaves them run-set only.
- **The group itself is not a judge.** Its name appears only as a subset argument; a bare `quality` is
  still a judge name. A member may sit in several groups.
- **Refused at startup**, each at its config path: an unknown member (listing the judges that exist), an
  empty group, a member listed twice, a group name that is not an identifier or is reserved by the
  grammar (`invariants`, an aggregate, `and`/`or`/`not`), an unknown group in a condition (listing the
  defined groups), and a literal threshold the group can never reach — `count_met(g) >= 3` on a
  two-member group (k > n).

**At the gate, a group is judged net.** The [regression gate](#session-isolation-mandatory-and-the-gate)
holds each DoD-set judge to "met before ⇒ met now". A judge that is in the DoD-set **only** through a
`done_if` group is judged with its group instead: the group regressed when more of its members went
met→unmet than unmet→met since the last landed state. So the quirky tool that flips while another
catches up is a swap, not a regression — which is what makes *k of n* worth having — while a group that
lost ground net is rolled back like any other regression. A member that is **also** named on its own
(bare in `done_if`, or in `invariants:`) stays under the per-judge rule as well. `any_regressed(g)` in a
condition is the per-member view (`met` once this run, unmet now), independent of the gate.

## Merge only on improvement — `merge_requires_improvement`

By default the gate merges every session that did not regress. A session that changes nothing the
judges care about (a reformat, a rename) still merges, and its diff costs review time without progress.
With `merge_requires_improvement: true` a session merges only if **nothing regressed and something
improved**:

```yaml
sequence:
  done_if: "tests_pass AND count_met(static_quality) >= 2"
  ratchet:
    cc_p95: { direction: "down", tolerance: 0.5 }
  merge_requires_improvement: true
  abort_if: "consecutive_no_improvements >= 4 OR consecutive_rollbacks >= 3"
```

- **Improved** means: a DoD-set judge was unmet at the last landed state and is met now; a `done_if`
  group gained ground net (more members unmet→met than met→unmet); or a ratcheted value moved the
  **good** way by more than its tolerance (`max(tolerance, |ref| × tolerance_pct / 100)`) — noise inside
  the tolerance is no more an improvement than it is a regression. An errored verdict improves nothing; a
  ratcheted judge with no reference yet cannot improve. A numeric judge that is not ratcheted counts only
  when its `met` flips — ratchet it to make its number count.
- **A rejected session** is not merged (its branch is kept for inspection, like a rollback's) and its
  rows in `verdicts.jsonl` say `"outcome": "no_improvement"`. It is **not a regression**: it never counts
  for `rollbacks` / `consecutive_rollbacks`, and it neither extends nor breaks that streak. It **does**
  count for [`stalled`](#stalled--the-shipped-stall-detector), as a step in which nothing moved. The gate
  counts it in its own terms, `no_improvements` and `consecutive_no_improvements` (since the last kept
  gate) — process-internal like `rollbacks`, so safe in `abort_if`.
- **Refused at startup** when there is no DoD-set judge and no ratchet: nothing could ever improve, so
  nothing would ever merge.
- Where it sits among the other rules that can stop a merge: see the
  [gate precedence](#session-isolation-mandatory-and-the-gate).

## The clock — three terms, all in seconds

`wall_time`, `human_wait_time` and `work_time` are condition terms *and* `limits:` keys. All three are
**seconds**, all three are computed by agg from its own state (so a worker cannot forge them), and
`wall_time` is **end-to-end across resumes** — it is read from a persisted start epoch, not from
process uptime.

| term | meaning | use it as |
|---|---|---|
| `wall_time` | now − run start, human waiting **included** | a **deadline** |
| `human_wait_time` | time accumulated inside blocking human calls | observability |
| `work_time` | `wall_time − human_wait_time −` time spent [waiting out a rate limit](#rate-limits--wait-wake-resume) | an **effort ceiling** |

```yaml
abort_if: "wall_time >= 86400"   # DEADLINE — 24h; a sleeping human counts against it
abort_if: "work_time >= 28800"   # EFFORT   — 8h of actual looping; a slow human or a closed quota window costs nothing
```

All three exist as separate terms because the [condition grammar](#the-condition-grammar) has
comparisons but **no arithmetic**: `wall_time - human_wait_time >= 28800` cannot be written, so the
difference has to be its own term. Which one you want is a real choice, and agg does not make it for
you: a run that must ship by tomorrow wants `wall_time`; a run that must not burn more than eight
hours of *agent* effort wants `work_time`, because ceilings keep firing while a
[human call](#asking-a-human-hil) is blocked and an overnight question would otherwise consume the
whole allowance.

> ⚠ **`wall_hours` was removed, not renamed.** The unit changed by 3600×, so a compatibility alias
> would let `wall_hours: 8` become `wall_time: 8` — an eight-**second** ceiling. agg hard-errors on
> the old key and prints the converted value. It also **warns** when a clock term is compared against
> anything under 60, because a fresh config has no old key to catch and `work_time >= 8` is almost
> always somebody thinking in hours.

## Asking a human (`hil`)

> **The default is that there is no human.** A project with no `hil_*` call in its driver and no
> opt-in paragraph in its `AGG.md` behaves exactly as it did before this feature existed: fully
> unattended, start to finish. Nothing below is on unless you turn it on, and that is the point —
> agg exists because a raw coding agent stops every few minutes to ask.

HiL is opt-in **twice over**, once per channel:

| channel | how it is turned on | can it stop the loop? |
|---|---|---|
| a **driver** `hil_*` call | by existing — a Rust author writes the call site | **yes**, it blocks there |
| the **worker**'s `agg hil` | by the project adding the paragraph [below](#letting-the-worker-ask) to its `AGG.md` | **no**, ever — it records and the session ends |
| `agg.yaml` | ⛔ not at all — there is no `hil` key | — |

Note the asymmetry in the last column, because it is the whole safety argument: the only thing that
can make the loop *wait* is a line of Rust a human wrote. A worker with the channel turned on can
page you, but it cannot stop the run, so the worst case is an unwanted notification and not a loop
that sits idle.

### Letting the worker ask

**Nothing tells the worker this exists.** The scaffolded `AGG.md` says *"There is NO human to answer
questions"* and leaves it there, deliberately: a worker shown an escape hatch in every session's brief
will reach for it, and a loop that asks instead of working is the failure this whole design is
against. Exactly like the [`blocked` judge](#blocked--the-workers-self-report-a-hint-not-a-fact), the
channel is real but unadvertised until **you** add it to your project's `AGG.md`:

```markdown
- If you hit something ONLY a human can resolve — a missing credential, a decision you are not
  allowed to make, a real-world action (provision an account, open a firewall, sign something) —
  ask through agg and END YOUR SESSION IMMEDIATELY: `agg hil bool "…"` / `agg hil choose "…"
  --option a --option b` / `agg hil input "…"`. It records and exits at once; the answer is at the
  top of your next brief. Do NOT guess, fabricate, or poll for it. This is a LAST RESORT — if you
  can make progress on anything else, do that instead.
- Never ask for a secret's VALUE. Ask for the credential to be PLACED (environment, keychain,
  `.env`) and confirm with `agg hil bool`.
```

Add it when your project genuinely has human-only steps in it (credentials, a real-world action, an
irreversible decision). Leave it out otherwise. Under `isolation: sandbox` the CLI still works — it
writes to `agg/state/asks/`, which is the worker's own directory.

Two guards apply, because a worker re-reads the same unresolved situation every session and *will*
ask again: a repeat of a question that is **already open** is dropped rather than paging you twice,
and at most **five** worker asks may be open at once. Neither is a permission boundary — a worker
cannot stop the loop whatever it does — they just stop the channel becoming a pager loop. An answered
question may be asked again, because the situation can genuinely recur.

### The worker: `agg hil` records and exits

A worker that hits something only a person can resolve runs one of:

```bash
agg hil bool   "Request firewall piercing for :443. Done?"
agg hil choose "Which store?" --option postgres --option sqlite
agg hil input  "Which instance is prod?"
```

These **never wait.** They record the question, print an id, and exit — a worker session is a paid
subprocess holding a git branch, so a worker that waited on a human would be the exact failure agg
exists to replace. The worker ends its session; agg pages you; the answer arrives in the **next**
session's brief, scoped to that id. There is deliberately no `--wait` flag.

⛔ **Never `agg hil input` for a secret.** The answer is written to the ask ledger and to the next
session's instructions, both files on disk. Ask for it to be placed where credentials go and confirm
with `agg hil bool`: *an answer may NAME a secret, never CONTAIN one.*

### The driver: `hil_*` blocks until answered

This is the channel that can stop the loop, which is why it is reachable **only** from Rust, only at a
call site a human wrote, and never from `agg.yaml` (there is no `hil` key — the YAML path never
blocks). A driver with no `hil_*` call runs unattended, exactly like every driver written before this
existed:

```rust
let i  = agg.hil_choose("Which store?", &["postgres", "sqlite"])?;   // -> usize
let v  = agg.hil_input("Which instance is prod?")?;                  // -> String
let ok = agg.hil_bool("Deploy to prod?")?;                           // -> bool
```

They block until a human answers. **No timeout, no default, no ending the run** — an idle agg process
spends no tokens, and waiting is cheaper than the machinery that avoids waiting. Two things make that
safe: `agg stop` and Ctrl-C interrupt a wait (the bus is drained on every poll), and `work_time` does
not count the waiting. Full detail in [docs/RUST_API.md](RUST_API.md).

### Answering

```bash
agg status                        # lists every open ask, its age, and the command to answer it
agg answer <id> "value"           # any ask. For a choice: an option, or its 1-based number
agg answer <id> yes               # a yes/no ask takes yes/no (or 1/2)
agg answer <id> 2 --json          # for a program: {schema_version, accepted, reason, id, answer}
```

`--json` prints the result as [`docs/schemas/answer.v1.json`](schemas/answer.v1.json) whether or not
the answer was accepted: `accepted`, `reason` (why it was refused, else `null`), the `id` as given and
the `answer` as recorded (an option number resolved to its text, `null` when refused). A refused
answer exits **1** with or without `--json`.

An answer to a `choose`/`bool` ask must be **on the recorded list** — anything else is refused at the
CLI with the options re-printed, and the ask stays open. That closed answer set is why `hil_choose`
exists next to `hil_input`, which is open and should be paired with a judge instead. The first answer
wins: a second one is refused rather than silently rewriting a decision the run may already have
acted on.

An answer is recorded in `agg/private/asks.jsonl` — [agg-owned](#state-and-memory), carved out of
the worker's writable set under `isolation: sandbox`/`container`. **That asymmetry is the point:** the
*request* may be worker-authored and is untrusted text, but the *answer* provably came from outside
the worker, which is what makes "a human approved the prod deploy" mean anything.

It is deliberately **not** put on the steering bus. An answer is a durable fact that outlives the
workflow that asked — a worker asks, its session ends, the workflow may reach its goal and exit while
the question is still open — so it must not depend on something running to be recorded. That is why
it is `agg answer`, a peer of `agg send` rather than a member of it, and why answering needs no
running workflow while [every `send` does](#the-bus-only-exists-while-a-workflow-runs).

### The bus only exists while a workflow runs

`agg send` steers a **running** workflow. Sending with none running is an **error** naming the missing
prerequisite, not a silent enqueue — the files can sit on disk with nothing listening, but a steering
message with nothing to steer is a landmine: a `stop` written now would fire at the startup of
whatever runs next, hours later, with nobody connecting the two. Anything stale is **purged** when a
workflow starts (archived to `bus/log.jsonl` first, so it is visible rather than silent).

The rule lives in one place that every channel goes through, so the CLI and `POST /api/send` cannot
disagree about it. Answering is exempt only because it is not a `send` at all — see above.

> ⚠ **A forgotten ask waits forever.** That is the accepted cost of having no timeout. `notify.cmd`
> fires when an ask opens, `agg status` shows it with its age, and `agg stop` ends a run nobody
> intends to answer. A worker-opened ask cannot hang anything — it never blocks the loop.

## Rate limits — wait, wake, resume

A subscription limit is a time window, not a budget. When a worker session ends on one ("You've hit
your session limit · resets 2:20pm"), agg treats that session as **incomplete**: it is never judged and
never merged. agg then:

1. **saves its work** — tracked edits, new files, and any commits the worker made — under
   `refs/agg/partial/<run_id>/<session>` and resets the tree to where the session started. Nothing is
   lost and nothing leaks into the next session. `git diff main refs/agg/partial/<run>/<n>` shows it;
   `git cherry-pick`/`git checkout <ref> -- <path>` takes it.
2. **waits** until the reported reset + 90 s (or `ratelimit_backoff_secs` when no reset was named), then
   starts the next session. Still limited ⇒ it detects that and waits again.

**The wait is visible.** `state.json` / `agg status --json` carry
`waiting: {reason: "rate_limit", agent, until}` (`until` = epoch seconds; `null` while working), and
`agg/private/waits.jsonl` gets a `wait` row when it starts (with the partial ref, if any) and a
`wait_end` row when it ends — `how`: `elapsed` · `woken` · `stopped` · `interrupted` · `ceiling`.

**The wait drains the bus** — a waiting loop is a running workflow:

| command | during the wait |
|---|---|
| `agg send wake` | end the wait and start the next session now — but no sooner than `ratelimit_min_gap_secs` (default 60) after the limited attempt, so an eager wake never hammers the provider |
| `agg send resume` | the same as `wake` while waiting (and unpauses, as always, otherwise) |
| `agg stop` / `agg send stop` | end the run: exit **5**, `outcome: stopped` |
| anything else | kept, in order, for the next session boundary |

Outside a wait, `agg send wake` is a logged no-op.

**Waiting is never a limit.** Its seconds are excluded from `work_time` (like a human's), and nothing
about waiting ends the run: without a signal agg waits as long as the provider requires. A ceiling
that trips on its own while waiting — a `wall_time` deadline — still ends it.

**Exit contract.** Exit 0 means only "`done_if` met". SIGINT/SIGTERM or `agg stop` during a session
or a wait exit **5** with `outcome: stopped`; a run that ended while waiting has `waiting` in its
[report](#the-run-report-aggprivatereportjson).

### `agg run --resume`

Continues the project's last interrupted run — the same `run_id`, session counter, ratchet state and
call ledger. It reuses the [driver's call-ledger resume](RUST_API.md): the YAML loop records a `step`
row per session and a `gate` row per judged session in `agg/private/calls.jsonl`, and a resume keeps
them back to the **last kept gate** — the only point it is sound to skip to, because everything after
it was never merged. From there:

- the sequence walk continues at the step after that gate (an `agg.yaml` whose `sequence.steps` no
  longer matches the ledger is refused: re-run without `--resume`);
- the ratchet references and gate history are those of that gate;
- session numbers **continue** after the interrupted session `n` — the resumed run starts with `n+1` —
  and tokens, cost and the clock are the interrupted run's, so a resume never launders a ceiling.

The interrupted session's work is on its partial ref, not judged. A run killed hard mid-session
(SIGKILL, power cut) left it in the tree instead; `--resume` saves it to the ref then. Refused when
there is no previous run, or when the last one met `done_if`.

### What a human's answer does NOT do

A human's answer unblocks the **step**; a **judge** still owns the **verdict**. Never let a "done"
satisfy a goal directly — re-run the check that looks at the world:

```rust
while !agg.judge(&dns_ok).met() {
    agg.hil_bool("Create the A record for billing.prod. Done?")?;
}
```

A human sign-off that *binds* `done_if` — a verdict row rather than a branch — is not built. See
`internal/HUMAN_LOOP.md` §7.3 for why it needs two extra rules first.

## Stuck detection and notification

A loop gets stuck two ways: a **hard blocker only a human can resolve** (a missing credential, an
ambiguous requirement, a call the agent isn't allowed to make), or a **soft stall** — it keeps running
but stops making progress. agg detects both with machinery you already have, because **a stuck detector
is just a judge**: it emits `{met, value, rationale}`, resolves by name from disk, and gets named in a
condition, exactly like `coverage`. There is no detection engine, no confidence model, no `kind:` tag.

The only net-new clause is **`notify_if` — the non-terminal twin of `abort_if`**. Same grammar, same
validator, the same startup hard error on a typo'd judge name. What differs is what happens when it is
true: `abort_if` stops the run; `notify_if` runs `sequence.notify.cmd` and **the loop keeps running**.

> **The anti-goal, and it is load-bearing.** agg exists because a raw coding agent stops every few
> minutes to ask a human what to do. "Stuck ⇒ stop ⇒ ask" is a regression to exactly that. So the loop
> **never waits for a human**: notification is a *side-channel*, not a gate. Stopping stays available —
> it is spelled `abort_if`, it is explicit, and it is never the mechanism by which a human is involved.

The zero-authoring version, which runs as-is because `stalled` ships inside the binary:

```yaml
sequence:
  steps: ["worker"]                # NO `stalled`-bounded repeat — see the escalation ladder below
  notify_if: "stalled"
  notify:
    cmd: ["curl -s --max-time 10 -d {{reason}} ntfy.sh/my-topic"]
```

### The three human policies — which clause the condition sits in

There is no `human:` block and no mode switch. The policy is **which clause the condition sits in** —
the same detector, the same delivery, one word of difference:

| `notify_if` | `notify.cmd` | policy |
|---|---|---|
| set | non-empty | **notify + keep going** — fires `notify.cmd`, exit code unchanged, **loop continues** |
| absent | non-empty | **stop + notify** — no live ping; it fires once when `abort_if` halts, including an `abort_if` already true at launch |
| absent | absent | **no human at all** — pure autonomy |
| set | absent/empty | ✗ **hard error at startup** — nothing would fire, so agg refuses the silent no-op |

### The escalation ladder is composition, not a config object

"flat progress → try a different-vendor recovery step → still stuck → notify and keep running → hard
ceiling → stop" is not one object you configure. Stage 1 is a **sequence** entry (a `max`-bounded
`until:` repeat that falls through to a recovery step), stage 2 is **`notify_if`**, stage 3 is
**`abort_if`** — two things that already existed plus the one new clause.

Stage 1 is composition too: an entry with `until:` stops repeating the moment its condition holds,
so the *next* entry is reached only when it did **not** hold within `max` tries. That fall-through is
the recovery dispatch — no `if:` needed, and it cannot be skipped by a lap where nothing was true.

**The ordering emerges from the detectors you choose — so choosing them is on you.** `notify_if` is
evaluated at the **gate of the session that just ran**; an `until:` condition is evaluated when the
**next** session is picked. Name the *same* detector in both and you are paged one full cycle
**before** the recovery step is even dispatched — the ladder inverted. Give `notify_if` the
**stricter** detector (a higher threshold, or `stuck` over `stalled`) so it can only be true after
the recovery already failed to move anything.

```yaml
steps:
  worker: {}
  reconsider: { agent: "codex", role_prompt: "Step back — the current approach is likely wrong." }

sequence:
  steps:
    - { step: worker, until: "NOT stalled", max: 3 }   # STAGE 1a — up to 3 tries, stop early if moving
    - reconsider                        # STAGE 1b — reached only when 3 tries stayed stalled:
                                        #            recovery, different vendor, no human involved
  done_if:   "all_tests_pass AND coverage.value >= 80"
  abort_if:  "over_budget OR work_time >= 28800"  # STAGE 3 — stop, on ceilings the worker can't reach
  notify_if: "stuck.value >= 85"                 # STAGE 2 — notify, KEEP RUNNING. STRICTER than the
                                                 # `stalled` repeat above, so it fires only after it failed
  notify:
    cooldown_sessions: 5
    cmd: ["curl -s --max-time 10 -d {{reason}} ntfy.sh/my-topic"]
```

> ⚠ **This example does not start until you author `stuck`.** `stuck` is a *user-authored* judge
> ([below](#the-three-stuck-detectors)); a name that resolves to no file is a **hard error at startup**.
> For zero authoring use `notify_if: "stalled"` — and then **drop the `until: "NOT stalled"` bound**
> (a plain `{ step: worker, times: 3 }`), or the same detector sits in both clauses and the ladder
> inverts.

### `notify.cmd` and the cooldown

```yaml
notify:
  cmd:                                    # shell commands, run in order
    - "curl -s --max-time 10 -d {{reason}} ntfy.sh/my-topic"
    - "timeout 10 ./agg/notify.sh {{project}} {{session}}"
  cooldown_sessions: 3                    # default 3. Minimum sessions between two notify_if fires.
```

`notify.cmd` runs **exactly like a hook**: foreground, in order, via `sh -c`, output to the loop log,
and **best-effort** — a command that is missing, fails, or exits non-zero is logged and never kills the
run. A notification is auxiliary; the loop's job is the loop.

> ⚠ **Foreground means untimed. Bound every command yourself.** Delivery is synchronous on purpose —
> the page goes out before the loop moves on — and agg imposes **no timeout**, so a `curl` against a
> host that accepts and then stalls hangs the gate until a human notices. Write
> `curl --max-time 10`, or wrap anything else in `timeout 10 …`. An unbounded delivery turns a
> side-channel into the [anti-goal](#stuck-detection-and-notification): a loop waiting on a human
> channel. (Same exposure as every other hook — this is just the one you point at the network.)

`cooldown_sessions` debounces `notify_if` only. One agg **session is one step execution**, so
`cooldown_sessions: 3` means "at most one ping per three steps" — the point is not to nag a human awake
on every cycle of an overnight run. `0` fires on every qualifying cycle. **The `abort_if` ping ignores
the cooldown** and does not consume it: a halt happens once, and it is the message you most want.

### The `{{…}}` variables — agg quotes them, so you must not

| var | value |
|---|---|
| `{{reason}}` | one line saying *why* — see below |
| `{{project}}` | the top-level `project:` |
| `{{session}}` | the session number |
| `{{step}}` | the step that just ran (empty string if there was none) |

```yaml
cmd: ["curl -s -m 10 -d {{reason}} ntfy.sh/my-topic"]      # ✅ agg POSIX-shell-quotes it for you
cmd: ["curl -s -m 10 -d '{{reason}}' ntfy.sh/my-topic"]    # ✗ the message arrives wrapped in ' '
```

The quoting is not cosmetic, and it is why you must not fight it: `{{reason}}` is frequently
**worker-authored** — the `blocked` detector's rationale is a line the *worker* wrote — and the command
is executed by `sh -c`, so shell-quoting is the single thing that stops a worker-written string from
becoming worker-written **code**. Two smaller rules: an **unknown** placeholder is passed through
verbatim rather than blanked (a silently empty `curl -d` looks delivered and says nothing), and
substitution is a **single pass**, so a value that happens to contain `{{project}}` is not re-expanded.

### What `{{reason}}` actually says

| fired by | `{{reason}}` is |
|---|---|
| `notify_if` | the `rationale` of **one judge named in the expression**: judges reporting `met` are preferred over the rest, the highest `value` wins inside that group, and a tie goes to the **first** in run-set order. An empty rationale is skipped. So `stuck.value >= 85 OR blocked` reports the firing 0–1 `blocked` rather than a quiet 0–100 `stuck`. |
| `notify_if`, no usable rationale | the **`notify_if` expression text** — when every named judge's rationale is empty, or the expression names only run-scalars (`over_iterations`, `work_time`). |
| an `abort_if` halt | the **`abort_if` expression text**, plus that same winning rationale when the expression names a judge: `blocked OR over_iterations — BLOCKED: need the prod deploy key`. A ceiling-only expression (`over_budget OR work_time >= 28800`) names no judge, so it arrives as just the expression. |

**It is a heuristic, not an attribution.** agg does not evaluate *which subterm* made the expression
true — `met`-first is a proxy for "this detector has something to say". A judge whose own `met`
threshold differs from the one you wrote (`stuck.value >= 50` against a rubric that sets `met` at 85)
can still lose to a met judge that is not the reason you were paged. Put detectors in one `notify_if`
only if you would want to hear from either of them.

Because that text is worker-authored, agg normalises it before it reaches you: whitespace collapses,
control characters are stripped, and it is capped at 400 characters. `{{reason}}` is always **one
line** — every sink you would send it to (ntfy, syslog, `>> file`, a phone notification) is
line-oriented, and none of them should be repaintable by the worker.

### Isolation: `notify.cmd` runs in the current step's jail

`notify.cmd` gets the [`isolation`](#isolation--blast-radius-jail-a-different-axis-from-session-isolation)
tier of the step that just ran — the same rule as hooks, for the same reason: it lives in `agg/agg.yaml`
inside the worker's writable cwd and typically execs a project script, both of which a confined worker
could rewrite.

- **`sandbox`** — the delivery is confined too. A command agg **cannot** confine is **SKIPPED, loudly**,
  never run unconfined. Writes are limited to the project dir + `$TMPDIR`, so a notify command that
  writes outside it (a log in `~`) fails. Network is fully open on this tier, so `curl` still works.
- **`container`** — hooks are host tooling on this tier (see the isolation table above), so
  `notify.cmd` runs on the host, unconfined.

If confinement is in your way, put the delivery on an `isolation: none` step, or move it to
`hooks.on_stop`, which fires once at the end with the run-level tier and no step context.

### `done_if` success does not notify

A finished run is not a request for help, so `notify.cmd` does **not** fire when `done_if` is satisfied.
"Ping me when the run ends, however it ends" is already spelled `hooks.on_stop` — it runs once on every
ending, success or abort:

```yaml
hooks:
  on_stop: ["curl -s --max-time 10 -d 'agg finished' ntfy.sh/my-topic"]
```

### The moat — a worker-authored signal belongs in `notify_if`, not `abort_if`

`blocked` reads `agg/state/BLOCKED.md`, a file the **worker** writes. Put that judge in `abort_if` and
the worker can end its own run by declaring itself blocked — precisely the failure agg exists to
prevent, and the same reason a worker never grades its own `done_if`.

```yaml
notify_if: "blocked"                     # ✅ the worker can ask for a human; the loop keeps working
abort_if:  "blocked OR over_budget"      # ⚠ …and now the worker owns the kill switch
```

**The discipline:** wire worker-authored signals to `notify_if` **only**. The worst-case abuse is then
"the worker made the loop ping a human" — annoying, rate-limited by the cooldown, and adjudicable (an
LLM `blocked` judge can read `BLOCKED.md` skeptically, as untrusted evidence). The loop keeps running;
it is never "the agent ended the run."

**Only one of the three tiers is actually out of the worker's reach.** Be precise about which:

| tier | example terms | what stops the worker |
|---|---|---|
| **process-internal** | `over_budget`, `over_cost`, `over_iterations`, `wall_time`, `human_wait_time`, `work_time`, `any_regressed(invariants)`, `rollbacks`, `consecutive_rollbacks`, `oscillating`, `ratchet_regressed`, `<judge>.ref` | agg computes them from its own in-memory run state. Nothing on disk. **Genuinely unfakeable** — these are what termination should stand on. |
| **agg-owned file** | `stalled`, `stuck` — both read `agg/private/verdicts.jsonl` | **depends on the isolation tier.** Under `sandbox`/`container` this is a real **permission** boundary: the ledger lives in `agg/private/`, which is carved out of the worker's writable set, so a write is refused by the kernel. Under the default `isolation: none` it is only a **protocol** boundary — agg writes that ledger and a worker has no sanctioned reason to touch it, so appending is unambiguously tampering, but nothing stops it and the ledger carries no integrity check. Forging three flat `merged` rows makes `stalled` report met. |
| **worker-authored** | `blocked` over `agg/state/BLOCKED.md` | nothing, on any tier. `agg/state/` is the worker's own directory — the worker is *supposed* to write this file. |

`stalled`/`stuck` are a **meaningfully higher bar** than `blocked` on every tier — the worker must break
the protocol rather than use it, and that is visible after the fact — and under `sandbox`/`container` the
bar becomes a wall the kernel holds. Row 1 is unfakeable regardless of tier, because agg never puts those
values on disk at all. Detect on any row; **terminate on row 1 unqualified, or on row 2 if you are running
confined and accept that the moat is only as strong as the tier you configured.**

**agg does not enforce any of this.** There is no startup guard refusing a worker-writable judge in
`abort_if`, and no integrity check on the ledger: "stop when the worker says it is blocked" is a
legitimate policy for a run you are watching, and the config is yours. It is a tradeoff you make
explicitly, not one agg makes for you.

### Rollbacks and oscillation — the gate's own detectors

Besides stagnation (`stalled`), two failure shapes need a stop rule in any long loop: **damage** — the
worker keeps making things worse and the gate keeps rolling it back — and **oscillation** — it fixes A
and breaks B, then fixes B and breaks A. agg answers both with **built-in terms**, not judges:

| term | type | what it counts |
|---|---|---|
| `rollbacks` | number | sessions the regression policy rolled back this run (boolean or ratchet regression). |
| `consecutive_rollbacks` | number | rollbacks since the last kept gate. |
| `no_improvements` | number | sessions [`merge_requires_improvement`](#merge-only-on-improvement--merge_requires_improvement) refused this run — never counted as rollbacks. |
| `consecutive_no_improvements` | number | no_improvement sessions since the last kept gate; a rollback neither extends nor breaks it. |
| `oscillating` | bool | the unmet set after a judged session repeats one seen within the last `oscillation_window` (default 4) judged sessions, with a **different** set in between: A→B→A. |
| `ratchet_regressed` | bool | the most recent judged gate recorded a [ratchet](#the-ratchet--numeric-no-regression) regression. |

```yaml
abort_if:  "consecutive_rollbacks >= 3 OR oscillating"   # unfakeable: safe in abort_if
notify_if: "rollbacks >= 2"                               # or just tell a human
```

**Why terms and not library judges like `stalled`.** `stalled` reads `verdicts.jsonl`, which is only a
protocol boundary under `isolation: none`. These are computed from what the gate decided, in agg's own
memory — the same tier as `over_iterations` — so they are unfakeable on every tier and belong in
`abort_if` without the caveat. They are persisted to `agg/private/gate.json` only so a resume can
restore them. Requirement-style spellings like `rollbacks.value >= 2` are refused with the right one:
a built-in number has no accessor, so write `rollbacks >= 2`.

The details that decide whether they fire when you expect:

- **Which sessions count.** `rollbacks` counts sessions whose staged merge the regression policy
  discarded. A session with no changes, a `red_file` veto or a merge conflict is not a rollback; it
  neither extends nor breaks the `consecutive_rollbacks` streak; nor does a `no_improvement` session,
  which counts for `no_improvements` instead. A kept gate resets both streaks.
  `skip_judges` steps are not gated, so they count for neither. With `gate_regressions: false` nothing is
  ever rolled back, so both stay `0`.
- **The signature** `oscillating` compares is the sorted set of **DoD-set** judges that were unmet (or
  errored) after the session, plus every judge that regressed its ratchet. Run-set-only detectors
  (`stalled`, a `notify_if` judge) are not part of it — a detector flipping is not a state of the work.
  Every judged session contributes one, **merged, rolled back or no_improvement** (a discarded session
  still showed where the worker went).
- **Short history.** Fewer than three judged sessions can never oscillate. An all-green signature never
  counts — returning to a good state is not oscillation — and A→A→A is a stall, not an oscillation.
  A monotonically improving run never trips it.
- **Shadowing.** These are grammar terms, so there is no file to shadow. A project judge with one of
  these names cannot be referenced — the built-in wins; name your own detector differently
  (`flapping`, …) and name that in the condition instead. The window is `sequence.oscillation_window`.

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
  "evidence":  ["…"]        // optional — files the verdict cites; agg KEEPS a copy (see Evidence)
}
```

A judge that emits no `value` is treated as **binary**; one that emits a `value` shows partial
progress. A judge that exits non-zero but prints a valid verdict is accepted. A judge that
crashes / times out / prints garbage is an **`error`** — never counted as a regression.

### Script judges

Run from the **project root** (cwd = project root, stdin = `/dev/null`), with env `AGG_SESSION`,
`AGG_STEP`, `AGG_JUDGE`, `AGG_PROJECT_DIR`, `AGG_JUDGE_SCRATCH` set. Just a file that prints the
verdict:

```bash
#!/usr/bin/env bash
# agg/judges/coverage.sh
pct=$(coverage report | awk '/TOTAL/ {print $NF}' | tr -d '%')
met=$([ "$pct" -ge 80 ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":100,"target":80,"rationale":"%s%% covered"}\n' "$met" "$pct" "$pct"
```

#### ⚠ A judge MAY NOT WRITE THE PROJECT TREE

Under `isolation: sandbox` (or `container`) a judge reads everything and writes **only**
`$AGG_JUDGE_SCRATCH`. The project tree is read-only to it, and `agg/private/` + `agg/judges/` are
denied outright. The reason is short: a judge that can write the tree it grades can edit its way to
a pass, which is the same hole as letting a judge choose its own writable paths.

**agg does not guess which folders your judge needs — it relocates the writes.** Before every judge
it points the standard toolchain variables at a scratch directory, so the common cases need no
change from you:

| variable | scope | for |
|---|---|---|
| `AGG_JUDGE_SCRATCH`, `TMPDIR` | per **session**, shared by that step's judges | your own temp files, and measurement hand-off |
| `CARGO_TARGET_DIR`, `GOCACHE`, `XDG_CACHE_HOME`, `PYTHONPYCACHEPREFIX`, `npm_config_cache` | per **project**, persistent | build caches — kept across sessions so nothing rebuilds cold |
| `PYTEST_ADDOPTS=-p no:cacheprovider` | — | pytest insists on a rootdir cache dir and has no variable for it |

Two consequences worth knowing:

- **A judge that still writes in-tree gets `EPERM` and fails loudly.** That is intended — it is a bug
  whether or not a sandbox catches it. Export your own variable from `$AGG_JUDGE_SCRATCH`, or copy
  what you need there and work on the copy.
- **The scratch is SHARED between the judges of one step**, which is how a *measure → threshold* pair
  works: one judge runs the benchmark and writes `bench.json`, another reads it and applies the
  ceiling. It is fresh each session, so a stale measurement can never be read as current.

Under `isolation: none` nothing is confined and the variables are still set, so a judge written for
the sandbox works identically either way.

#### Evidence — what agg keeps

The scratch does not last, so a file a verdict cites there would be gone by the time someone asks
why the gate passed. After every judging pass (the baseline, each judged session, each driver
`agg.judge()`), agg **copies** each file in a verdict's `evidence` list to

```
agg/private/evidence/<run_id>/<session>/<judge>/<path>      # <session> is `baseline` for the baseline pass
```

```bash
# agg/judges/complexity.sh — measure into the scratch, cite the file
lizard src --xml > "$AGG_JUDGE_SCRATCH/cc.xml"
printf '{"met":true,"value":%s,"rationale":"…","evidence":["cc.xml"]}\n' "$n"
```

- **Resolution.** A relative path is looked up in the judge's scratch first, then the project root;
  an absolute path is taken as is. It is kept under its path relative to whichever of the two roots
  holds it.
- **Confinement.** The path — with every symlink resolved — must lie inside the scratch or the
  project, and be a regular file. `../../.ssh/id_rsa`, `/etc/hosts`, or a scratch symlink pointing out
  of both roots is **refused**, never copied: a judge reads worker-written content, and its verdict
  must not turn into a copy agg makes of something outside the work.
- **The cap.** `judge.evidence_max_mb` (default 100) bounds what one run copies. A file that does not
  fit is skipped with a warning; the run goes on.
- **Where it is recorded.** Each `verdicts.jsonl` row carries `evidence_saved` (the kept copies,
  project-relative) and `evidence_skipped` (`<declared>: <reason>` for each entry not kept). The run
  report lists the kept copies on each verdict (`judges.<name>.last.evidence`, `…baseline.evidence`,
  absolute) and names the run's directory in `paths.evidence_dir`.

agg never deletes kept evidence; clear `agg/private/evidence/` yourself when it is no longer needed.

**Which tier applies to a judge?** The RUN's, never the tier of whichever step invoked it — a judge is
an evaluator, and the paths a worker must not change are exactly the ones a judge needs to read and
execute. The run's tier is the **strongest** any step declares: in YAML that is `defaults.isolation`
or any step's `isolation:`; in a Rust driver it is `.isolation(..)` on any `Step` it builds. So
sandboxing one step confines every judge in the run, which is deliberate — otherwise the judge fired
after an unconfined step would be the way out.

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

### `stalled` — the shipped stall detector

`stalled` is the detector behind `until: "NOT stalled"` and the zero-authoring `notify_if: "stalled"`. It
reads `agg/private/verdicts.jsonl` and is **met** when, across the last **K = 3 MERGED steps**, no binary
judge changed its `met` and no numeric judge changed its `value`. The details that decide whether it
fires when you expect:

- **Only `merged` rows count.** A rolled-back step's churn was undone, so it must read as neither
  progress nor stall. **Except `no_improvement` rows** ([`merge_requires_improvement`](#merge-only-on-improvement--merge_requires_improvement)):
  the gate itself found that nothing improved, so such a step joins the window carrying the state of
  the step before it — a step in which nothing moved.
- **Like every discarded session, a `no_improvement` session's own verdicts — `stalled`'s included —
  are reset to base truth after the gate**, so within an unbroken no_improvement streak `stalled` cannot
  turn met; it fires on the next kept session. To bound a streak itself, use the gate's term:
  `abort_if: "consecutive_no_improvements >= 3"`.
- **`stalled` ignores its own rows** — it cannot stall on itself.
- A judge with **fewer than two rows** in the window is ignored; it cannot have changed.
- **Fewer than K qualifying merged steps ⇒ not met.** It refuses to call a stall on thin history, so it
  is silent at the start of a run.

**K is hardcoded**, because library judges are parameterless. To change it — or the whole heuristic —
drop your own `agg/judges/stalled.sh`: a project file **shadows** the library by name. Or write
`stalled_hard.sh` and name that instead, leaving `stalled` intact.

It is **binary** (it emits no `value`), so name it bare — `notify_if: "stalled"`, not `stalled.value`.
The shipped implementation shells out to `python3` to parse the ledger; shadow it if you don't have one.

### The three stuck detectors

The [stuck-detection](#stuck-detection-and-notification) ladder is built from three judges. Only the
first one ships:

| detector | ships? | signal | can the worker fake it? | put it in |
|---|---|---|---|---|
| `stalled` | ✅ embedded in the binary | judge values flat over the last 3 merged steps | **not without tampering**, and not at all under `sandbox`/`container` — `verdicts.jsonl` lives in the carved-out `agg/private/` | `if … then`, `notify_if` |
| `stuck` | ✗ **you author it** | a 0–100 confidence over agg's own ledger | **not without tampering** — same file, same tier-dependence | `notify_if` (or `abort_if`, if you accept [the moat](#the-moat--a-worker-authored-signal-belongs-in-notify_if-not-abort_if)'s middle row) |
| `blocked` | ✗ **you author it** | the worker declaring a human-only blocker | **yes** — the worker writes the file | `notify_if` — see [the moat](#the-moat--a-worker-authored-signal-belongs-in-notify_if-not-abort_if) |

**A config naming `stuck` or `blocked` is a hard error at startup until the file exists.** Only
`stalled` works out of the box.

#### `blocked` — the worker's self-report (a hint, not a fact)

```bash
#!/usr/bin/env bash
# agg/judges/blocked.sh — WORKER-AUTHORED evidence. Wire it to notify_if, not abort_if.
[ -s agg/state/BLOCKED.md ] \
  && echo "{\"met\":true,\"value\":1,\"max\":1,\"target\":1,\"rationale\":\"$(head -1 agg/state/BLOCKED.md | sed 's/\\/\\\\/g; s/"/\\"/g; s/[[:cntrl:]]/ /g')\"}" \
  || echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"no blocker declared"}'
```

The `sed` escapes backslashes, then quotes, then flattens control characters to spaces. All three
rules are load-bearing: the rationale is a line the *worker* wrote, and JSON forbids a raw tab, CR or
ESC inside a string just as firmly as an unescaped quote. Emit any of them and agg scores the judge
as an **`error`** — false, not a blocker — so the one channel a worker has for reaching a human fails
silently and closed on an everyday indented line. The quote rule is also what stops the file's
content from forging `met` or `value`: `","met":false` arrives as text, not as JSON.

Plus one line in the project's `agg/AGG.md`, so the worker knows the channel exists:

> If you hit something ONLY a human can resolve (missing creds, an ambiguous requirement, a decision
> you are not allowed to make), write one line describing it to `agg/state/BLOCKED.md` and stop — do
> NOT guess or fabricate.

`agg/state/` is gitignored runtime state, so the file survives a session rollback — and a crash, and a
reboot. Delete it once you have unblocked the run: a stale `BLOCKED.md` under `abort_if: "blocked"`
halts the *next* launch at the baseline judge pass (it pings, but it never gets a session in).

#### `stuck` — a confidence score over agg's own ledger

`agg/private/verdicts.jsonl` is the append-only record of every verdict — session, step, the verdict
fields, the gate decision. A rubric judge reads it through the existing `log:<path>` input, so there is
no new plumbing:

```markdown
---
inputs: ["log:agg/private/verdicts.jsonl", "log:agg/private/LOG.md", "diff"]
---
You are a STUCK detector. From the verdict history (each session's judge values over time), the
memory log, and the latest diff, score 0–100 how confident you are that this loop is GENUINELY
STUCK — judge values flat for several sessions, diffs churning with no new insight, notes repeating
the same plan — as opposed to slow-but-progressing or doing deep work.
Output `value` = that confidence; `met` = (value >= 85).
```

A **script** version is equally valid and cheaper — e.g. `python3` over `verdicts.jsonl` computing "max
goal value unchanged for N sessions" → a confidence. Either way it reads **agg-owned** data instead of
a worker-authored file, which is the higher bar of [the moat](#the-moat--a-worker-authored-signal-belongs-in-notify_if-not-abort_if)'s
middle row. Putting `stuck` in `abort_if` is a reasonable way to let the loop give up on its own — under
`isolation: sandbox`/`container` the kernel is what stops the worker from writing that ledger; under the
default `none` you are trusting it not to.

## Choosing an agent

`agent:` (in `defaults` and `judge`, and overridable per step) picks which coding agent runs. They
are **not interchangeable**, and a config that asks for something an agent can't do is **refused at
startup** — checked for **every** agent the sequence names. The full matrix is in the README under
[Choosing an agent](../README.md#choosing-an-agent).

### Agent-specific rules

| | rule |
|---|---|
| `model:` | **Codex: omit it** (naming a model you aren't entitled to is a hard 400). **Copilot: `auto`.** Applies to both `defaults.model` and `judge.model`. |
| `effort:` | Levels `low<medium<high<xhigh<max`, per-step (unset ⇒ the backend's default). **Defaults: Claude `max`, Codex `high`, Copilot none** — so the two you'd pick for a hard task are *already at their ceiling* without setting anything. Claude's `max` is the highest reachable **headlessly**: the top `ultracode` tier is interactive-only (not a valid `-p` value), so a worker can't select it — `max` is the ceiling. **Codex** clamps `xhigh`/`max`→`high` (it has no level above `high`). **Copilot** cannot combine `effort:` with `model: auto` (its default) — agg refuses the pair; name a concrete model to use an effort. |
| `limits.cost` / `over_cost` | **Claude only.** Codex reports no dollars; Copilot bills in AI Credits. **Checked per step** — even one `agent: codex` step makes a `limits.cost` guard uncoverable, so agg refuses it. Use `sequence.limits.tokens`; Copilot can self-cap with `worker_args: ["--max-ai-credits", "50"]`. |

## Session isolation (mandatory) and the gate

Every session **always** branches off `base`, does its work, and is gated — there is no master switch.
`agg run` therefore **refuses to start** without a git repo, a clean tree, and a non-detached HEAD.
The gate rule: **auto-accept a session's work, UNLESS a judge that was previously met now fails, or a
[ratcheted](#the-ratchet--numeric-no-regression) value got worse beyond its tolerance — then roll that
session back** (`gate_regressions: true`). Four things can suppress a merge, in this
precedence (plus a fifth, opt-in):

0. **a protected-path violation** — the session changed a [`write_deny`](#protected-paths--write_deny-readonly-writable)
   path ⇒ roll back, regardless of `gate_regressions`, of `merge_requires_improvement` and of every
   verdict. It counts as a rollback.
1. **`red_file`** (`.agg_red` at the project root) — the worker's own veto. Present ⇒ do not merge. Not a
   rollback and not a `no_improvement`: nothing was staged to decide about.
2. **`skip_judges`** — nothing was judged, so nothing merges; the work stages. The next judged step's
   gate decides the whole span, and that is where 3 and 4 look — a skip step's work can improve (or
   regress) only as part of its span.
3. **the regression gate** — a previously-met judge now fails (a `done_if` [group](#judge-groups--k-of-n):
   lost ground net), or a ratchet regressed ⇒ roll back (`gate_regressions: true`). With
   `gate_regressions: false` the regression is kept, and rule 4 still applies.
4. **`merge_requires_improvement: true`** — nothing improved ⇒ do not merge, record `no_improvement`.
   Applied only to a session no earlier rule stopped: a session that regressed AND improved nothing is a
   **rollback** (rule 3 wins), so it counts for `rollbacks`, never for `no_improvements`.

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

Everything agg **reads** as config is under `agg/` (committed). Runtime state is split across **two**
gitignored, auto-created directories — and the split is by **who may write them**:

| directory | written by | contents |
|---|---|---|
| **`agg/state/`** | the **worker** (agg reads it as untrusted input) | `STATE.md`, `wiki/`, `sessions/`, `spawns.json`, `spawns/`, `BLOCKED.md` |
| **`agg/private/`** | **agg only** | `INSTRUCTIONS.md`, `LOG.md`, `state.json`, `project.json`, `verdicts.jsonl`, `calls.jsonl`, `gate.json`, `clock.json`, `violations.jsonl`, `waits.jsonl`, `protected/`, `evidence/`, `prompts.jsonl`, `bus/`, `run.pid`, `run.log`, `report.json` |

**The one rule that decides which is which:** *if the worker writing it could change when the loop ends,
what it may spend, or what agg believes happened, it is private.* Everything the worker is **supposed** to
author lives in `agg/state/`.

- **`agg/AGG.md`** (committed) — the **stable** scope/goals/architecture the worker reads for
  orientation. Human-owned, rarely edited; this is where `AGG_STATE.md`'s stable header content now lives.
- **`agg/state/STATE.md`** — the forward state file (`what to do next`). **Worker-curated**: the worker
  rewrites this advice each session. Gitignored, so it **survives a session rollback** — the code
  attempt is thrown away, the advice about it is not.
- **`agg/state/wiki/`** · **`agg/state/sessions/`** — the worker's durable knowledge base and its
  transient per-session scratch notes. Worker-owned by design.
- **`agg/state/spawns.json`** · **`agg/state/spawns/`** — the long-task registry and per-spawn logs.
  Worker-writable because `agg spawn` is a command the **worker** invokes; its blast radius is bounded
  (what to reap, what to tell the next session — nothing that gates the run).
- **`agg/private/INSTRUCTIONS.md`** — regenerated by **agg** every session; it is the worker's **entire
  `-p` input**. The worker's `-p` is a tiny fixed pointer ("read `agg/private/INSTRUCTIONS.md` in full and
  follow it"); agg composes the file from operator steering, the step's role framing + its `prompt:`, a
  recent-tail excerpt of memory, pointers to `STATE.md` and `AGG.md`, the wiki, and a standing footer.
  Private *even though the worker reads it every session*: it is the worker's **orders**, and a worker
  able to rewrite its own brief mid-run could launder instructions past you.
- **`agg/private/LOG.md`** — durable institutional memory (`what we tried and rejected`). Written
  by **agg**, never the worker — never tell the worker to maintain it. The worker's contribution arrives
  as a scratch note in `agg/state/sessions/` that agg sanitizes on the way in; a direct edit would bypass
  exactly that sanitizing.
- **`agg/private/state.json`** — the live scoreboard snapshot (the TUI, `agg serve`, `/agg:status` read it).
- **`agg/private/verdicts.jsonl`** — the append-only, safety-critical GATE record.
- **`agg/private/prompts.jsonl`** — per session, the SHA-256 of every [prompt file](#prompts-from-files--prompt_file-role_prompt_file)
  its brief was read from. Private because it is the audit trail of what the worker was told.
- **`agg/private/evidence/`** — the files each verdict cited, kept after its judge ran
  ([Evidence](#evidence--what-agg-keeps)). Private because a verdict CITES them: a worker able to swap
  a kept file would rewrite why a gate passed.
- **`agg/private/project.json`** — the run-history ledger (`agg history`): lifetime sessions/tokens.
- **`agg/private/run.pid` · `run.log`** — the loop's liveness, and its log: every run stamps its
  `run_id` here, and `--detach` / `--report-json` send the whole process output here.
- **`agg/private/report.json`** — the end-of-run report, the stable interface for programs that
  embed agg. See [below](#the-run-report-aggprivatereportjson).
- **`agg/private/bus/`** — the steering queue (`agg send …` writes here; the loop drains it at `INJECT`,
  and every second during a [rate-limit wait](#rate-limits--wait-wake-resume)).
  Private because the bus is the **operator's** channel: a worker writing here would raise its own token
  ceiling with `agg send budget`, unpause itself, or inject its own next-session instructions.
- **`agg/private/waits.jsonl`** — every rate-limit wait: its start (agent, `until`, partial ref) and how
  it ended.

### What the split actually buys you — and on which tier

`agg/state/` sits inside the worker's cwd, so under `isolation: sandbox` the worker could write **every**
file in it. Three of those files decide when the loop ends or what it costs — `verdicts.jsonl` (the ledger
`stalled`/`stuck` read), `bus/` (steering + budget), `run.pid` (the double-run guard and `agg stop` target).
Moving them under one subpath lets agg carve that subpath **out of the sandbox's writable set**: a
`deny file-write*` rule after the allow on macOS (Seatbelt), a read-only rebind on Linux (bubblewrap), a
`:ro` remount in the container tier. It is derived from the worker's cwd inside the wrapper, so it covers
every confined spawn — worker, script judge, and `agg.yaml` hook alike.

> **This binds only under `isolation: sandbox` and `isolation: container`.** Under the default
> `isolation: none` the worker has your whole filesystem and **no directory layout changes that** — it can
> write `agg/private/` as easily as anything else. The layout is what *makes the confinement expressible*;
> the isolation tier is what enforces it. See
> [`isolation`](#isolation--blast-radius-jail-a-different-axis-from-session-isolation).

**Reads are untouched.** The carve-out denies writes only. The worker still reads its brief from
`agg/private/INSTRUCTIONS.md`, a `stuck` judge still reads `log:agg/private/verdicts.jsonl`, and the TUI
still reads `state.json`.

### Migrating an existing project

The `agg/state/` → `agg/private/` move is **breaking** — agg does not migrate for you, and a run started
against the old layout starts with an empty ledger and no run history.

```bash
cd <project>
mkdir -p agg/private
# nothing under agg/state/ is tracked, so a plain mv is the whole migration
mv agg/state/{verdicts.jsonl,state.json,project.json,run.pid,run.log,LOG.md} agg/private/ 2>/dev/null
mv agg/state/bus agg/private/ 2>/dev/null
rm -f agg/state/INSTRUCTIONS.md          # regenerated every session; no need to keep it
```

Leave `STATE.md`, `wiki/`, `sessions/`, `spawns.json`, `spawns/` and `BLOCKED.md` where they are — those are
the worker's. `INSTRUCTIONS.md` is regenerated every session, so dropping it instead of moving it is fine.

Then update anything of **yours** that names a moved path: a `stuck`/`stalled` judge's
`inputs: ["log:agg/state/verdicts.jsonl", …]`, a `notify.cmd` or hook that greps the ledger, CI that tails
`run.log`. A judge pointed at the old path reads an empty file and scores `error`, not a loud failure —
grep your `agg/` for `state/` once after the move.

Both directories are gitignored. `agg run` appends `agg/private/` to your `.gitignore` on startup if it is
missing, alongside the `agg/state/` entry it has always written — an existing project picks the new entry up
on the next run. It writes the same entries to the repository's `.git/info/exclude` too: on a base branch
that never committed a `.gitignore`, session 1 commits the one agg wrote, and a rollback of that session
deletes it again — `info/exclude` is outside the work tree, so no checkout, merge or reset can remove it. `agg/` itself stays committed: the judges must be in git so a rollback can restore a grader
a worker tampered with.

`memory:` keys: `enabled` (default true), `max_kb` (cap on the stored file), `inject_kb` (how much is
injected per prompt). `0` for either disables that cap.

## The run report: `agg/private/report.json`

Every ending agg can observe writes one report: `done_if` met, `abort_if` fired, a session cap, an
operator stop (`agg stop`, SIGTERM, SIGINT), an error, and a panic inside agg itself. It is the
**stable** interface for a program that embeds agg — unlike `state.json`, `verdicts.jsonl` and
`project.json`, which are internal and change without notice.

```bash
agg run --report-json | jq -r .outcome      # logs go to agg/private/run.log; stdout is the report
```

| field | meaning |
|---|---|
| `schema_version` · `agg_version` · `run_id` · `project` | identity. `run_id` is fresh per run (kept by `agg run --resume`) and also in `run.log`. |
| `outcome` | **authoritative**: `done` · `aborted` · `stopped` · `error`. |
| `exit_code` | what `agg run` exited with: 0 done · 3 `abort_if` · 4 session cap (`outcome: aborted`) · 5 stopped · 1 error. |
| `stop_reason` · `error` | human-readable reason (the `abort_if` expression, the cap, the stop reason); the error chain or panic message when `outcome` is `error`. |
| `started_at` · `finished_at` | epoch seconds. |
| `sessions` | `total`, `merged`, `rolled_back` (discarded: a regression, a veto, a conflict), `no_improvement` (not merged because nothing improved — [`merge_requires_improvement`](#sequence--the-loop)), `staged_unmerged` (staged on a span no gate decided). |
| `time` | `wall_time_s`, `work_time_s`, `human_wait_time_s` — the [clock](#the-clock--three-terms-all-in-seconds). |
| `judges` | by name: `last` and `baseline` verdicts (`met`, `value`, `max`, `target`, `rationale`, `error`, and `evidence` — the absolute paths of the [kept](#evidence--what-agg-keeps) evidence copies, omitted when none); `null` where there is none. The driver path has no baseline. |
| `result` | `base_branch`, `final_commit` (the base's tip after the last gate), `staged_branch` (an ungated span's tip — `git merge` it to land that work). |
| `open_asks` | questions still waiting on a human: `id`, `kind`, `question`, `options` (the allowed answers of a `bool`/`choose` ask; `null` for `input`), `age_s`. |
| `paths` | absolute paths of `report`, `ledger` (`verdicts.jsonl`), `log`, `evidence_dir` (this run's `agg/private/evidence/<run_id>`; `null` when no evidence was kept). |
| `detectors` | the stop detectors as the run ended: `stalled` (the last verdict of a judge named `stalled`; `null` if there is none or it never ran), `oscillating`, `rollbacks`, `consecutive_rollbacks`, `ratchet_regressed`. |
| `usage` | the run's totals: `total`, `worker`, `ruler` — each `{tokens, cost_usd, cost_source}` (see [usage](#usage-tokens-and-cost)). |
| `session_log` | every session that made an agent call, in order: `{session, step, usage: [{role, agent, model, tokens, cost_usd, cost_source}]}`. Session `0` is the baseline pass (its LLM judges); `step` is empty there. One line per role and model: `worker` is the session's agent, `ruler` the LLM judges and the summarizer. |
| `waiting` | a wait the loop was in when it ended — `{reason: "rate_limit", agent, until}` (`until`: epoch seconds) — else `null`. See [rate limits](#rate-limits--wait-wake-resume). |

Each judge's entry also has `ratchet`: `{reference, direction, tolerance, tolerance_pct}` for a
ratcheted judge (`reference` = its value at the last kept gate, or the seed), `null` otherwise. `last`
is `null` only for a judge that never ran — an unmet judge reports its verdict.

### Usage: tokens and cost

agg runs the agents, so it records what each call **reported** — never an estimate, never the
configured model:

| field | meaning |
|---|---|
| `model` | the model the agent says it ran (Claude: `system`/`init`, or `modelUsage` on a one-shot; Copilot: `data.model`, the model `auto` resolved to). `null` when the agent never names it (Codex). |
| `tokens` | `input` (uncached prompt), `output`, `cache_write` (prompt written to the cache), `cache_read` (prompt served from it). The four do not overlap. A kind the agent does not report is `null`, not `0`: Copilot reports `output` only; Codex has no `cache_write`. In a sum, a kind is `null` only if no call reported it. |
| `cost_usd` | dollars as the agent priced itself — the API-equivalent price, not a subscription bill. Only Claude reports one. |
| `cost_source` | `reported` (every call in the figure priced itself) · `partial` (some did — `cost_usd` is their sum, a lower bound) · `unknown` (none did — `cost_usd` is `null`). |

Recording usage never caps a run. `agg history --json` carries the same `usage` totals for every
run; `agg status --json` shows them live.

**Outcome precedence** — when several hold at the end of one session: `error` > `stopped` > `done` >
`aborted`.

**Guarantees.**
- Written **atomically** (temp file, fsync, rename): a partial report is never readable.
- Deleted when a run **starts**, so a report on disk is always the latest run's — never a stale one.
- A refused start (bad config, unknown `--base-branch`, dirty tree) still writes an `error` report.
  So does a panic. If the report cannot be written, `agg run` exits **1** with the reason on stderr.
- **SIGKILL, an OOM kill or a power cut leave no report.** A consumer MUST treat a missing report as
  `outcome: error`.
- The schema is published at [`docs/schemas/report.v2.json`](schemas/report.v2.json). Any change to
  the shape bumps `schema_version` and ships a new file. `schema_version` 1
  ([`report.v1.json`](schemas/report.v1.json), agg 0.0.15) is superseded: v2 adds `detectors`,
  `usage`, `session_log`, `waiting`, each judge's `ratchet` and each open ask's `options`, and
  removes nothing.

## The live status: `agg status --json`

Where a run stands **now**, for a program watching it from another process (`agg --project <dir>
status --json`). It is cheap: it reads the snapshot the loop publishes, the ask ledger and
`run.pid`, and never runs a judge. Its field names are the report's wherever the two say the same
thing, so one parser reads both. Schema: [`docs/schemas/status.v1.json`](schemas/status.v1.json).
Exit 1 with a hint when no run has ever published a snapshot in the project.

| field | meaning |
|---|---|
| `schema_version` · `agg_version` · `run_id` · `project` | identity. `run_id` is the one the report will carry. |
| `running` | a loop process is alive for this project right now. |
| `finished` · `stop_reason` | the run published its ending, and why (`null` while it runs). Neither `running` nor `finished`: it was killed. |
| `phase` | where the loop is: `starting` · `inject` · `run` · `verify` · `gate` · `backoff` · `staging` · `done`. |
| `current_step` | `{name, agent, model}` of the step being run (`model` is what agg asked for, `null` = the agent's default); `null` before the first. |
| `session` · `lifetime_session` | the session number in this run, and across every run of the project. |
| `time` · `sessions` | as in the report; `time` ticks while the run is alive. |
| `judges` | by name: `last` (latest verdict, `null` until it ran), `in_dod` (counted toward `done_if`), `ratchet` (as in the report). |
| `detectors` · `open_asks` | as in the report (`open_asks` with `options`). |
| `usage` | the running totals `total` / `worker` / `ruler`. The per-session breakdown is in the report. |
| `waiting` | `{reason, agent, until}` while the loop waits before its next session — an agent's rate limit (`reason: "rate_limit"`, `until` the reset in epoch seconds or `null`) — else `null`. |
| `paths` | as in the report. |

Values change between two reads only at stage boundaries (and `time` continuously): the loop
publishes on every stage change, and a worker session's usage lands when the session ends.

`agg status` without `--json` is for humans, and its text changes without notice.

## Environment overrides (CI-friendly)

These override the config at load time:

| env var | overrides |
|---|---|
| `AGG_MODEL` | `defaults.model` (not a step that names its own model) |
| `AGG_COST_TOTAL` | `sequence.limits.cost` |
| `AGG_TOKEN_BUDGET` | `sequence.limits.tokens` |
| `AGG_HEARTBEAT_SECS`, `AGG_WATCHDOG_IDLE_SECS`, `AGG_WATCHDOG_CPU_GRACE`, `AGG_RATELIMIT_BACKOFF`, `AGG_MEMORY_MAX_KB`, `AGG_MEMORY_INJECT_KB` | the matching top-level keys |

> **Platform note.** `agg` is **unix-first** (macOS + Linux). The Windows binary builds and the core
> outer loop runs, but two safety features are **not** implemented there: the **CPU-flat half of the
> watchdog** and **process-group reaping**. `agg run` prints a one-line notice on Windows.
