# Changelog

Notable changes to agg. Newest first; each entry names the config keys and interfaces it touches.

## Unreleased

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
