# Changelog

Notable changes to agg. Newest first; each entry names the config keys and interfaces it touches.

## 0.0.36 (2026-10-26)

### Changed

- **Every workflow session starts fresh** (`agg::workflow`): a step's first session, its repairs and its
  refreshes no longer continue or fork an earlier agent session. What a step knows is in its brief and the
  project's files. A continued session took its brief for known and could carry an earlier instruction into a
  new task; on a cold cache it also paid for its whole history again. Removed: `Task::context`,
  `workflow::forget` and the session store `.git/agg-sessions.json`. `Step::resume` and `Step::fork` stay on the
  driver API.
- **A refresh needs declared inputs:** after a re-entry, only a step with `Task::inputs` is a refresh (unchanged
  inputs: no session; else a brief led by the changes since its result, now to its inputs alone). A step without
  inputs can't tell a refresh from a new task, so it runs its task whole.
- **The worker's brief carries the critic's rubric once:** "How your result is reviewed" quotes the rubric as
  written, without the worker's own task, which the critic's copy appends and the brief already holds.

### Added

- **The error that ends a workflow run is in its history:** an error a driver returns past every block (e.g. a
  file it can't write) is recorded as kind `error` against the step running then (`workflow::run`).
- **`Step::tools(names)`:** limits a worker to these tools of its agent; its MCP servers' tools come on top.
  Claude: `--tools a,b` (a session then starts with only those tools' descriptions); an agent that can't limit
  its tools runs with all of them and the step says so (`AgentBackend::tool_args`).
- **`AGG_CLAUDE_SETTINGS`:** Claude Code's `--settings` (a file or its JSON) for every Claude worker that runs
  without the user's settings, e.g. a hook that shortens its shell commands' output.

## 0.0.35 (2026-10-26)

### Added

- **`AGG_MODELS`:** model stand-ins from the environment (`<model>=<stand-in>`, comma-separated): every
  session, judge and summary that names `<model>` calls `<stand-in>` (`backend::overrides::model`), e.g. Opus
  for Fable where a provider has no Fable.
- **`AGG_CODEX_CONFIG`:** Codex `-c` overrides from the environment, one `key=value` per line, on every Codex
  session and one-shot (`backend::overrides::codex_config`): a Codex judge reads no config file, so this is how
  it reaches a provider other than OpenAI's (e.g. Amazon Bedrock).
- **`AGG_CLAUDE_MCP_CONFIG`:** Claude Code's `--mcp-config` (a file or its JSON) for every Claude worker that runs
  without the user's settings, since such a worker loads none of the user's MCP servers. E.g. a web search
  where the provider has none.

## 0.0.34 (2026-10-26)

### Added

- **`Flow::trust(rule)`:** triage's proposals that the driver's rule accepts (the failed step, the step to
  re-enter) go ahead without a human's yes, still within the run's re-entries, recorded as trusted: for a
  failure the driver knows is the re-entered step's own to repair.

### Fixed

- **A step re-entered with a note starts a fresh session:** with `--from <step> --note <text>`, the step's
  first session in the run no longer continues (or forks) its old agent session, which remembered the result as
  done, skimmed the brief, missed the note and reported done again. Its repairs continue the new session;
  refreshes and repairs elsewhere continue theirs as before.
- **A continued session's opening message carries what is new:** `Step::news(text)` names what is new for an
  attempt; a session that resumes its own (not a fork) gets it in its opening message after the pointer to the
  brief, cut at 4000 characters with a pointer to the brief (`loop_::opening`, `ResolvedStep::news`). The
  workflow's leads (the review to address, the changes since the last result, the note) set it through
  `workflow::with_lead`.
- **A flow's scratch folder is removed when the flow ends:** the folder is now `agg-flow-<pid>-<n>` in the temp
  dir, one per flow, removed when the `Flow` is dropped (normal end or error), read-only files and all; a flow
  that starts removes the folders of processes that are gone. A dead run's files no longer read as current to
  a later run's agent.

## 0.0.33 (2026-10-24)

### Added

- **A driver can send its run back itself:** `Flow::reenter(key, note)` ends the run and starts it again at a
  step the run has seen, with the note, as after a triage a human said yes to; it counts as a re-entry and is
  recorded `reentry`. A step the run hasn't seen is refused. Without it, only triage could re-enter.

## 0.0.32 (2026-10-24)

### Added

- **Workflow variables:** `Agg::vars([(name, value), ..])` fills in `{{ name }}` in every step's prompt file
  (the role prompt's too) and every LLM judge's rubric, its `inputs:` frontmatter included, where agg reads
  them; a name the map lacks fails that step or judge (`unknown variable`). Only files are templates: inline
  prompts a driver builds may carry a project's own `{{ … }}`, and `{{` without a plain name inside stays as
  it is. `agg::prelude::render(text, &vars)` renders a driver's own files the same way. Without variables
  nothing changes.

## 0.0.31 (2026-10-22)

### Added

- **A task can climb:** `Task::climbs()` makes an unmet end return `Outcome::Unmet(why)` to the caller, the
  work kept (gated and merged), and recorded as `unmet`, instead of failing into triage. The caller runs its
  next attempt (a stronger model, more repair rounds, another tool) on what this one left. Without it, nothing
  changes: an unmet task fails into triage as before.

## 0.0.30 (2026-10-21)

### Fixed

- **The opening message names the session and step:** every worker is now told "… your complete brief for
  session N, step `name`" instead of the same line every time. A session forked or resumed from an earlier
  step's (`context`) got exactly the message its parent got, took the old brief for the new one and answered
  from memory without opening `agg/private/INSTRUCTIONS.md`. `loop_::INSTRUCTIONS_POINTER` became
  `loop_::instructions_pointer(session, step)`.

## 0.0.29 (2026-10-21)

### Fixed

- **Nothing to score is met:** a score judge on changed files (`ChangedIn`) whose changed files no part of the
  definition applies to (e.g. only a shell script changed: every component `not_applicable`, no gate failing)
  is vacuously met, like the existing "no changed files in scope". It used to fail as "the score is incomplete
  (null)". A part that failed to measure (`unavailable`) or a failing gate still fails closed.

## 0.0.28 (2026-10-21)

### Fixed

- **A condition's flip is no regression:** `Agg::condition` runs, records and memoizes a verdict exactly like
  `Agg::judge`, but keeps it out of the gate's regression set; `Flow::applies` asks its condition with it. A
  condition flips with the world (a tag that exists now and didn't when the last gate landed), and the gate
  took that flip for a regression and rolled back the next step's work. A judge the span also asked by name
  stays held.

## 0.0.27 (2026-10-19)

### Added

- **A score of the build's reports:** `judges.<name>.score.reports` and `ScoreJudge::reports`: a script that
  writes the build's test, coverage, mutation and class reports before the measurement and names them; agg
  hands them to swe-metrics (`--report kind=path`), so a score includes test coverage, mutation score and
  dependency cycles. Fails closed when the script fails or a report is missing.

## 0.0.26 (2026-10-19)

### Added

- **A score of a sub-path:** `judges.<name>.score.scope` (`target` or a project-relative path, never `changed`)
  and `ScoreJudge::scope`: the score measures that path as its own target, against that path's incumbent. A
  workflow scores the code it builds without the rest of the project.
- **What a step changed in a path:** `Scope::ChangedIn { path, since }` (driver only) measures the files of
  `path` changed since the Git ref `since` — a workflow step's own changes, from the ref it began at. A score
  of it has no incumbent to compare with, and is vacuously met when nothing changed.
- **A missed score names its costliest parts:** the rationale ends with the (at most three) quality components
  that cost the most points, with their value and the definition's good and bad thresholds, so a repair
  knows where to start.

## 0.0.25 (2026-10-18)

### Added

- **Workflow building blocks** (A-26 to A-31, `agg::workflow`), moved from AMF's driver where they ran fifteen
  measured runs: `Flow` (a run with `--from`/`--note`/`--reset`/`--until`, a step history, hooks, triage),
  `script_step`, `applies`, `task` (an agent step with checks, a critic, repairs, a script first, refresh by
  inputs, sessions resumed or forked), `approval`, `workflow::run` (re-entry after triage) and `Roles`.
  A library on the driver API: no second execution model. Parallel sessions (A-32) are not part of it.

## 0.0.24 (2026-10-18)

### Added

- **A system prompt for every worker** (A-34, `Agg::system_prompt(text)`): a rule that must hold for the
  whole session, where the brief is read once in the first turn and fades over a long session. Claude
  `--append-system-prompt`; `Capabilities::supports_system_prompt` (Claude only). An agent that can't take
  it runs without it and the step says so. None by default.

## 0.0.23 (2026-10-17)

### Added

- **A step can start from an earlier step's agent session** (A-33): `StepOutcome::agent_session` names the
  agent and its session id; `Step::resume(&s)` continues that session, `Step::fork(&s)` branches off it and
  leaves it unchanged (Claude `--resume <id> [--fork-session]`). Only the same agent can; on another agent,
  a backend without resume or fork (`Capabilities::supports_fork`, Claude only), or a session the agent no
  longer has, the step starts fresh and says why. The YAML path still starts every session fresh.
- **Workers without the operator's own agent settings** (`Agg::user_settings(false)`): their plugins, MCP
  servers, hooks and personal instructions stay out of the run's sessions (Claude `--setting-sources
  project,local`; `Capabilities::supports_project_settings_only`). On by default, as before.

## 0.0.22 (2026-10-14)

### Fixed

- **Driver:** a `skip_judges` step whose session committed nothing reports `Landing::Nothing`, as documented,
  instead of `Landing::Span` whenever an earlier session had opened the span. The session is still staged,
  so the gate keeps counting the span as a no-change one (`no_changes`).
- `agg status` and the driver's human-ask prompt name the command that answers an ask, `agg answer <id>`
  (they said `agg send answer`, which does not exist).

## 0.0.21 (2026-10-13)

### Added

- **Agent, model and effort per LLM judge** (`Judge::agent(Agent)`, `Judge::model(m)`, `Judge::effort(Effort)`
  on a `Judge::rubric`). The judge asks that agent instead of the run's ruler — a reviewer that is not the
  worker's agent, sized for the job. With its own agent and no model it uses that agent's default judge
  model; `Agent::Custom` is a failed verdict. Its spend counts toward the agent that answered. Unset, the
  call is the same as before.
- The ruler's one-shot call takes a reasoning effort: Claude and Copilot `--effort`, Codex
  `model_reasoning_effort` (clamped as for workers).

### Changed

- An LLM judge is asked for a rationale that lists every point to fix when its verdict is not met, so it
  can serve as a review.

### Fixed

- **Web UI:** the control panel is rendered once, outside the state branches. Two branch copies could sit in
  the DOM at the moment the state appeared, showing an action's feedback twice (a flaky browser e2e).

## 0.0.20 (2026-10-13)

### Added

- **Working memory per step on the Rust driver** (`Step::memory(bool)`, `Agg::memory(bool)`). With memory
  off, a session's brief names no forward state (`STATE.md`), no wiki and no memory log, its footer asks
  for neither, agg doesn't warn about an untouched `STATE.md`, and no memory fold or summary runs after it.
  Off never deletes memory: a later step with it on sees what earlier sessions left. `Agg::memory` is the
  run's default (on); a step's own setting wins. For a workflow whose steps run once and hand their results
  on in their own files. agg.yaml is unchanged.

## 0.0.19 (2026-10-13)

### Added

- **`Agent` and `Effort` parse from their YAML names** (`FromStr`): a driver that takes its agents as text
  (from its own config) passes them through, and an unknown name is refused with the valid ones listed.

## 0.0.18 (2026-10-12)

### Added

- **The Rust driver can say what the YAML `judge:` block, a step's `role_prompt*` and a fired
  `abort_if` term say.**
  - `Agg::judge_agent(Agent)`, `Agg::judge_model(m)`, `Agg::judge_timeout(secs)` — the ruler, YAML's
    `judge.agent` / `judge.model` / `judge.timeout`. `Agent::Custom` as the ruler is refused when the
    run boots (the ruler is resolved by name, as in YAML).
  - `Step::role_prompt(text)` / `Step::role_prompt_file(rel)` — YAML's per-step `role_prompt:` /
    `role_prompt_file:`. They replace each other, and a step setting either replaces the defaults'
    pair, exactly as a YAML step does; both on one step, or an unreadable file, is refused by
    `agg.step()`.
  - `Agg::finish_with(outcome, reason)` — end the run naming the stop condition the driver saw fire.
    For `RunOutcome::Halt` it is recorded as `agg run` records a fired `abort_if` term: report
    `stop_reason` `ABORT: <reason>`, run-ledger end reason `abort:<reason>`. `Agg::finish` is
    unchanged.

## 0.0.17 (2026-10-12)

### Fixed

- **A worker that changes nothing no longer idles forever under `abort_if: "stalled"`.** A session
  that changed nothing git-visible (no commit, so nothing to merge) used to be written to
  `verdicts.jsonl` as `rolled_back` and counted for nothing: not for `stalled` (which read only
  `merged` and `no_improvement` rows), not for any gate term. A worker that believed it was finished
  while a judge stayed unmet therefore ran session after session — one run reached 325. Such a session
  is now a step in which nothing moved:
  - its ledger rows carry the new outcome **`no_changes`** (not a rollback, not a "was met" reading);
  - the shipped **`stalled`** judge counts `no_changes` steps, with their own readings (they are base's:
    nothing was merged or discarded), so three idle sessions in a row are a stall;
  - two new gate-history terms, **`no_changes`** and **`consecutive_no_changes`** (since the last kept
    gate), count them — process-internal like `consecutive_rollbacks`, so unfakeable and safe in
    `abort_if`: `abort_if: "consecutive_no_changes >= 3"`. Driver: `agg.gate_state()`.
  - the report and `agg status --json` count them in **`sessions.no_changes`** (`report.v2` and
    `status.v1` schemas: a new required field).

## 0.0.16 (2026-10-11)

### Added

- **Network egress allow-list — `isolation_policy.network: open | { allow: [host[:port], …] }`.**
  `open` stays the default. `allow` confines the worker, its judges and the LLM judge's one-shot (and
  on `sandbox` the hooks) to the listed destinations through ONE enforcement point, a small threaded
  proxy in agg that checks the name a client asked for before resolving it, resolves allowed names
  itself and refuses loopback/link-local answers (DNS rebinding, the metadata address). The
  `container` tier is the reference: an `--internal` network whose only reachable address is a relay
  to the proxy, `NET_RAW` dropped. macOS `sandbox`: Seatbelt allows outbound TCP only to the proxy's
  loopback port (+ `localhost[:port]` entries), and no DNS. Linux `sandbox`, podman and `none` refuse
  an allow-list at startup — never silently open. Startup also refuses a list that does not hold
  each agent's own endpoint (`AgentBackend::api_hosts`: `api.anthropic.com`, `api.openai.com` /
  `chatgpt.com`, …) and warns about what it cannot cover (hooks on a container run, `on_start`,
  `background`). Adversarial tests on both tiers (direct IP, DoH, raw socket / UDP from a child, a
  worker-set proxy, plain DNS). `agg doctor --json` reports `network`. Driver: `Agg::network`. See
  `docs/CONFIG.md` → *Network egress* for the platform matrix.
- **The container tier runs the agent.** Under `isolation: container` the agent CLI itself runs inside
  the step's `image:` — as the project owner (`--user uid:gid`, never root), with a fresh `HOME` every
  worker session, and its credentials passed **by name** (`-e NAME`: `ANTHROPIC_API_KEY`,
  `CLAUDE_CODE_OAUTH_TOKEN`, `CODEX_API_KEY`, …) or mounted **read-only** (`~/.codex/auth.json`) — never
  baked into an image, never written to a ledger, log or the report. New keys:
  `isolation_policy.pass_env` (more names) and `isolation_policy.credentials` (more read-only mounts),
  `judge.image` (the judges' image). Reference images and the image contract: `docker/claude`,
  `docker/codex`, `docs/CONFIG.md` → *The container tier runs the agent*. Startup probes each image
  (as the project owner): an image without the CLI a step needs, or a mounted path the engine cannot
  see (colima shares only `$HOME` — a bind of anything else is an empty directory), is refused at its
  key; `agg doctor --json` reports `container_images`. A container step's agent no longer needs to be
  installed on the host. Driver: `Agg::image`, `Step::image`, `Agg::judge_image`, `Agg::pass_env`,
  `Agg::credentials`. Manual real-agent check: `scripts/e2e_container_real.sh`.
- **Read-only context directories — `context:`.** A list of `{ path, as, mode: ro }`: directories from
  outside the project (a knowledge base, reference material) that every confined process reads and
  none may change. `sandbox` keeps them read-only in the kernel (macOS: a Seatbelt write deny plus an
  unlink deny on each ancestor, so a context cannot be renamed away and replaced; Linux: `bwrap
  --ro-bind`), `container` mounts them `:ro` at `/context/<as>`. The brief lists each one with a pointer
  to its `index.md`; every confined process, judges included, finds it as `AGG_CONTEXT_<AS>`. Refused at
  startup, each at its key: a missing path, a file, an overlap with the project, a malformed or
  duplicate `as`, a `mode` other than `ro`, and any `isolation: none` step. Driver: `Agg::context(path,
  name)`. See `docs/CONFIG.md` → *Context directories*.
- **`agg wiki lint [PATH] [--json]`**: checks a knowledge wiki (default: the worker's
  `agg/state/wiki/`) against OKF v0.2. Problems, which fail the lint with exit 1: a concept without
  parseable frontmatter or a non-empty `type`; an `index.md` that is not link entries under headings,
  or carries frontmatter beyond a root `okf_version`; a `log.md` whose headings are not `YYYY-MM-DD`
  dates, newest first. Warnings: the shape of the optional `status`, `generated`, `verified`,
  `sources` and timestamp fields, and a `verified` key in the worker's own wiki. Read-only. JSON
  schema: `docs/schemas/wiki_lint.v1.json`.
- **`docs/BACKENDS.md`**: how to add an agent backend: the `AgentBackend` contract, the capability
  flags and what each refuses, stream and one-shot parsing, usage and model reporting, rate-limit
  detection, recorded fixtures and the required tests.
- **`docs/backends/kiro.md`**: the Kiro CLI was investigated as a backend and is not implemented. Its
  headless mode exists, but the event schema of its JSON stream, token reporting, rate-limit
  signalling and a read-only one-shot that ignores repo configuration are undocumented, and no
  stream could be recorded.
- **Judges built on swe-metrics** (A-20). A judge can be DECLARED in `judges:` instead of written as a
  file: `metric: { id, tool?, stat?, labels?, scope?, combine? }` reads one number of a
  [swe-metrics](https://github.com/ssenge/swe-metrics) report, `score: { definition, version }` its
  0–100 quality score, each held to `max:`/`min:`.
  - All metric and score judges of a session share ONE `swe-metrics measure` of the session's tree; the
    report and the tools' SARIF are kept as evidence.
  - Fail closed: a skipped or failed analyzer, or an absent metric, makes the judge error with the
    analyzer's reason — never met.
  - Several tools for one metric: `tool:` or `combine: strictest | all | majority`; with `all`, tools
    moving in different directions are no improvement for the ratchet (the verdict's new optional
    `mixed` field).
  - `scope: changed` measures only the files changed since the base branch; a sub-path scope measures a
    directory of the project. With no changed file the analyzer applies to (nothing changed, or only
    docs), the judge is vacuously met (`met: true`, no `value`, the verdict's new optional `vacuous`
    field) — no improvement, no regression, and not a "was met" reading at the gate. Real failures in
    the changed scope still error.
  - A score is computed against the incumbent's report (`agg/private/metrics/incumbent/`); a score
    capped by a failed gate names the gates; a definition/version mismatch is refused; a `null` score is
    an error with the score's notes; a changed definition resets the ratchet reference on `--resume`.
  - A ratchet on a metric or score judge may omit `direction` (the judge states it; the report's
    direction is checked against it).
  - swe-metrics stays optional: a config declaring such a judge is refused at startup when
    `swe-metrics` is not on `PATH` or speaks another schema version.
  - Rust driver: `Judge::metric(name, MetricJudge::new(id)…)` and `Judge::score(name, ScoreJudge::new(def, version)…)`.
- **Metric and score judges on the `container` tier** (A-11 × A-20). On a container run, `swe-metrics
  measure` / `score` run in `judge.image` like every other judge: the project read-only, writing only the
  session's judge scratch (`agg/private/container/judge/scratch-<n>/`) and a fresh `HOME` per call
  (`agg/private/container/home/metrics-*`), `TMPDIR` the container's `/tmp`, no cache that outlives the
  call; the run's network policy, context dirs and `pass_env` apply. They ran unconfined on the host
  before. The incumbent's report is read from the read-only project; a scoring definition file outside
  the project is mounted read-only; one measurement per session tree still holds. `agg judge` and
  `agg plan` measure in `judge.image` too on a container run.
  - Startup checks swe-metrics where it will run: on a container run, in `judge.image` (its presence,
    `swe-metrics schema report|score` `$id`, the score definition), refused at `judge.image` (else
    `defaults.image`) — the host no longer needs swe-metrics. `agg doctor --json` lists it under
    `container_images[].clis`. An image lacking a CLI is now reported at the key of what runs it there
    (a step's image vs `judge.image`) when both share one image.
  - trivy's vulnerability database is the host's, mounted read-only with updates off
    (`TRIVY_SKIP_DB_UPDATE`); the container downloads nothing. No database: a startup warning, and
    `dependencies.*` judges fail closed.
  - Reference judges' image `docker/swe-metrics/` (`build.sh <swe-metrics checkout>`): swe-metrics from
    one git commit plus its `tools.lock`-pinned tools, for any uid, tool update checks off.
    `scripts/e2e_metrics_container_real.sh` runs it end to end by hand.

### Changed

- **Judges run in the run's strongest tier, `container` included** — there is no opt-out. When any
  step is `isolation: container`, every script judge and LLM judge runs in `judge.image` with the project
  mounted read-only (scratch and toolchain cache under `agg/private/container/judge/`). They used to
  run on the host, unconfined, so a container worker could rewrite a judge script into a host escape.
  A container run's judges therefore need their toolchain in `judge.image`. Hooks stay host tooling.
- `isolation: container` runs every container as the owner of the project dir (`--user uid:gid`),
  no longer as root — files it writes into the project are the operator's (they were root-owned on a
  Linux host). An image must therefore work for a non-root uid (the image contract).
- `isolation: container` no longer mounts the host's agent state (`~/.claude`, `~/.codex`) into the
  container; the agent gets its own `HOME` instead.
- `agg run` / `agg check` require on the host's `PATH` only the agent CLIs that run on the host; a
  container step's agent is checked inside its image instead.
- The worker's brief now shows the reserved `index.md` and `log.md` shapes next to the concept
  example, says never to write `verified`, no longer suggests the OKF v0.1 `timestamp` key, and points
  at `agg wiki lint`. A worker that copies the examples writes a conformant wiki (tested). agg still
  writes nothing into the wiki.
- The brief's "start with" hint for the wiki lists `index.md` first when there is one.
- `sequence.ratchet.<judge>.direction` is optional for metric and score judges (still required for
  every other judge — refused at startup without it, now located at `sequence.ratchet.<judge>.direction`).
- `judges.<name>` accepts `metric`, `score`, `max`, `min` beside `timeout`.
