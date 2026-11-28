# Changelog

Notable changes to Relentless (called AgenticGoGo, binary `agg`, up to 0.0.51; the entries below 0.1.0 keep the old names). Newest first; each entry names the config keys and interfaces it touches.

## 0.6.0 (2026-11-28)

### Added

- **Forks: parallel work the Rust way.** `Relentless` is now `Send + Sync`, and **`rlt.fork()`** gives an instance of the engine in a
  worktree of its own, on a branch of its own cut from what the parent has kept (made at its first call); **`rlt.merge(fork)`**
  merges its branch back (`rlt: merge fork <name>`, a conflict is `Fatal::Conflict { branch, files }`); a fork that is dropped is
  discarded and its branch kept. The parallelism is plain Rust: `rayon::join`, `std::thread::scope`, a thread pool; the best of
  several tries and a lane that starts lanes are ordinary code. A fork shares the run's bus (`stop`, `pause`, `resume`, `wake` and
  `inject` reach every fork), its asks (`[fork <name>]`, answered with `rlt answer`), its lane views and its spend (the run's
  `limits` count every fork, and survive a resume; the run's report adds up what they reported); a fork resumes from its saved ledger (`rlt/private/forks/`). One thread works on
  an instance at a time (a second one panics and says to fork); an instance carries its variables and settings to the thread that
  runs it. `Relentless::fork_seed` copies ignored paths into every worktree. Ledger records `fork`, `merge` and `settled`. See
  `docs/RUST_API.md`, "Forks", and `examples/forks.rs`.
- **`kind: parallel`**: the children of a process step run at once as lanes, in the order they are listed joined (`parallel:` is
  optional and takes the keys of a parallel `each`'s: `max`, `fail_fast`, `seed`, `target`). Numbered and listed like an `each`'s
  children; a `kind: parallel` step inside an `each` runs once per item. `--check` refuses a checkpoint, `max: 0` and anything
  parallel inside it. See `docs/PROCESS.md`, "Different steps at once", and `examples/parallel-steps/`.

### Changed

- **A step's `checks:` is now `verifiers:`** (the vocabulary the rest of the engine uses); the old key is still read, so existing
  workflow files run unchanged. `task_force.checks` (commands) keeps its name.
- **The slogan** is "Agents that don't stop until the job is truly done" (README, `rlt --help`, the social card); the README is
  rewritten around a simple picture, the whole workflow as a diagram, and a first workflow in YAML and in Rust
  (`examples/hello`, `examples/fix_add.rs`).

### Changed (breaking)

- **`Relentless::fan_out`, `FanOut`, `LaneReport` and `LaneEnd` are gone**: fork and merge with any Rust parallelism instead. The
  string keys and the closure that matched on them are what went; ordering, a conflict and the choice of what to merge are now the
  driver's own code.
- The plugin traits `Handler` and `PreStart` and the values of the extension map are `Send + Sync`.

## 0.5.0 (2026-11-28)

### Added

- **Where lanes run.** `parallel.target` of a parallel `each`: `local` (the default), `container`, `ssh` or `command`. A lane that
  doesn't run in this process works on a standalone clone of the project (carrying the run's git identity, the seed, the workers'
  memory, a copy of the process's home and its spec), and **`rlt lane --spec rlt/private/lane-spec.json`** (a hidden command) runs it
  wherever the target started it, leaving `rlt/private/lane-result.json`. A target is four commands (`launch`, `poll`, `kill`,
  `fetch`) with `{lane_dir}`, `{key}` and `{lane}`; the run polls, brings the clone back, fetches the lane's branch and joins it as
  any lane. `ssh` and `container` write the commands for you (tar streams over ssh with `setsid nohup`; `docker run -d` with the clone
  mounted); `command` is the plug-in point (`docs/recipes/`: a plain ssh launcher, a disposable cloud machine). `pass_env` passes
  credentials by name: a file of mode 0600 inside the ssh stream, or docker's own environment passing; never a command line or a log.
  The operator's stop reaches a remote lane as `kill`; a resumed run polls a launched lane again instead of launching a second.
  `--check` refuses a target without what its kind needs and a setting of another kind. See `docs/PROCESS.md`, "Where lanes run",
  `examples/remote-session/`.
- **`rlt images build [--rlt <linux binary>] [<agent>…]`** builds the reference images (`docker/claude`, `codex`, `swe-metrics`), each
  with the agent's CLI and `rlt`, tagged `rlt-<agent>:<version>` and `:latest`; `rlt images list` and **`rlt doctor --containers`**
  say which engine and images there are; a container target whose image is missing names the command that builds it.
- **`rlt install-remote <host> [ssh options…]`** runs the install script on a host over ssh.

### Changed

- A remote `DOCKER_HOST` is not supported for the per-session container tier; lane targets are the way to run elsewhere (docs).

### Not built

- Shared services across machines (the rate-limit gate, `pause` and `inject`, live spend): a remote lane is stopped by `kill`, and
  its spend counts when it has ended. A real host and a real container run of an agent session have not been tried here: the ssh,
  container and command targets run in the tests against fake `ssh` and `docker` and a launcher that runs the lane on this machine.

## 0.4.0 (2026-11-28)

### Added

- **Parallel lanes.** `parallel: { max, fail_fast, seed }` on an `each` of a process file runs its items at once as lanes: each in a
  linked worktree of its own (`rlt/private/lanes/<each>-<key>/`) on a branch of its own (`rlt/<project>/lane-<each>-<key>`, its
  sessions `rlt/<project>-<each>-<key>/session-N`), with an engine instance of its own on a thread of its own. The run joins the
  finished lanes into its branch in item order (`rlt: join <each> <key>`), takes back the append-only files a lane kept apart
  (`run.history`, `run.notes`, `run.open_points`) and the workers' memory, aborts and names a conflict, resumes (`--resume` skips
  the joined lanes and resumes the others from their own ledgers), and forwards `rlt send` commands to the lanes, mirrors their asks
  into its own ledger (`<key>:<id>`) and publishes `rlt/private/lanes.json`, which `rlt status` shows as a table. A rate limit one lane
  meets holds back the others, and every lane's spend counts toward the ceilings of a driver's `fan_out` (a process file has none). `--check` refuses `max: 0` and a checkpoint
  inside. See `docs/PROCESS.md`, "Parallel lanes", and `examples/parallel/`.
- **`Relentless::fan_out(keys, FanOut, body)`** for a Rust driver: the same lanes for a list of keys; `FanOut`, `LaneEnd`, `LaneReport`
  in the prelude; `Relentless::project_name`, `Relentless::lanes` (what the scheduler sets). `examples/fanout.rs`, `docs/RUST_API.md`.
- `src/lanes/` (`worktree`, `schedule`, `join`, `services`, `forward`, `record`): `LaneServices` is what an engine instance calls (stop,
  rate-limit gate, charge and ceiling, views); `None` is an ordinary run and every call site keeps its code then.

### Changed

- **A failed branch cut ends the run** (it used to run the worker on the base branch without isolation after a log line).
- Git commands that meet a held lock (`config.lock`, a ref's `.lock`) are tried again, five times 200 ms apart; the git dir of a
  linked worktree is looked up (a stranded merge is found there; the verdict cache lives in the common dir and is written whole
  and moved into place).
- Process-wide state that a second run in the same process would share is per thread or per run: the worker groups a signal kills
  (every worker's, not the last), the task force's re-entry and spent-limit marks, `vars`, `user_env`, the leftovers sampler (one thread
  for the process; each run ends only its own steps' leftovers), the measurement cache (a lock per key, not one held across a whole
  measurement), the temp name of Codex's answer schema.
- The scenario harness takes a lock around choosing a scripted agent's rule and logging the call: a rule that answers `times: 1`
  answers one of many calls made at once.

### Not built

- `rlt send --lane <key>` (a command reaches every running lane), a lane record in `calls.jsonl` (the run keeps `lanes.jsonl` and a
  `lane` record in the process history), the lanes as parallel bars in `--report timeline` and as rows of the web process map,
  and a log per lane merged in item order for `expect` in a scenario.

## 0.3.0 (2026-11-28)

### Added

- **Any agent CLI, described in YAML: the command harness.** `agents: { <name>: { command, output, fields, capabilities,
  rate_limit_pattern, preflight, env, experimental } }` in `rlt.yaml` (and `rlt: { agents }` in a process file) makes a CLI an
  `agent:` like the shipped ones: `command` is the argv (`{brief_file}`, `{brief}`, `{workdir}`, `{model}`, `{session}` are
  filled in), `output: text | json_lines` with `fields` naming where the JSON lines hold text, output tokens, cost and session
  (JSON pointers, `type=/pointer` for one event type). It claims only the capabilities it is told, shows each output line, lets
  the exit code decide success, and is never the ruler (no read-only one-shot call). `worker_args` follow its command. A
  driver's own `AgentBackend` is launchable too (`Agent::Custom`); the worker takes a `BackendRef`.
- **Four experimental presets of it: `gemini`, `opencode`, `qwen`, `pi`.** Flags read off each CLI's `--help` (Gemini CLI 0.71.1,
  OpenCode 1.19.6, Qwen Code 0.28.7, Pi 0.73.1); the brief goes in a file and the prompt says to read it. OpenCode's stream was
  recorded, Qwen's and Pi's up to their "no login" error, the rest is synthetic (`tests/fixtures/agent-streams/`, README).
  Every run prints `agent <name> is experimental`, `rlt --version` and `rlt doctor --json` list them (`experimental: true`), and an `agents:`
  entry of the same name replaces a preset. `docs/BACKENDS.md` has the rule for synthetic fixtures. `examples/mixed-agents/`
  works with a command agent and Claude.

### Fixed

- The unit tests of the command harness compile (they used `unwrap_err` on types without `Debug`); the docs of `BackendRef` and
  of the `AgentBackend` trait are on their own items again.

## 0.2.0 (2026-11-27)

### Added

- **Retries, failure classes and a per-step `timeout`.** A worker session that fails for no fault of the work (an
  overloaded API, a dropped connection, a crash, a hang) is tried again inside the same step: `defaults.retry` / a step's
  `retry` / `Step::retry` take `{ attempts: 3, backoff: { initial: 15s, factor: 2, max: 5m, jitter: true }, on: [transient,
  crash, timeout] }` or `false`, and `timeout: 45m` kills a session that runs longer. A failure is classified
  ([`AgentBackend::classify`], default in `relentless::retry`): `transient`, `crash`, `timeout` are tried again by default;
  `rate_limit` waits for its reset as before; `auth` and `unknown` never. A retry is not a work session: one ledger record
  with `attempts: n` (absent means 1, old ledgers replay unchanged), no new session branch, nothing counted toward
  `stalled`, the repair budget or the dud streak; the failed tries' tokens and cost are charged; `rlt send stop` ends a
  backoff wait at once. Process files take `retry` and `timeout` in a step's settings. See `docs/CONFIG.md`, "Retries and
  timeouts".
- **`min_free_disk`** (`defaults` of the top level and `rlt: { min_free_disk }` of a process file; `Relentless::min_free_disk`):
  the least free disk a run needs, a size (`10GB`, the default), a share (`5%`) or `false`. It is checked on the project's and
  the temp folder's file system before each worker session, each round of verifiers and each driver lap, and in `rlt verify`.
  Below it the run stops with its cause, exit **7** (`RunOutcome::Disk`), report outcome `stopped`, and `--resume` goes on.
- **A temp folder per session.** Every worker session, script verifier and process script step gets its own short `TMPDIR`
  (`/tmp/rlt-<run>/<n>`; short because a unix socket path may have 104 bytes on macOS), removed when it ends
  (`RLT_KEEP_TMP=1` keeps it). It is writable under the sandbox, bound after bwrap's tmpfs on Linux and mounted in a container.

### Changed

- **The flow scratch moved into the project:** `rlt/state/flow/run-<pid>-<n>/` (was `$TMPDIR/rlt-flow-…`), where a confined or
  containerised worker can write the file its brief names. The verifier cache moved to the user's cache folder
  (`~/Library/Caches/rlt/verifier-cache/`, `$XDG_CACHE_HOME`/`~/.cache` or `%LOCALAPPDATA%`), because macOS purges `$TMPDIR`.
- `rlt verify` fails with the disk message instead of running a verifier on a full disk.

## 0.1.0 (2026-11-27)

**AgenticGoGo is now Relentless (`rlt`), and judges are now verifiers.** This is a hard rename: there is no compatibility
window, no alias and no `migrate` command. Nothing reads the old names, so a project created with `agg` does not load
with `rlt` (its run state, ledgers and branches use the old names). Start such a project fresh, or rename it by hand with
the table below.

### Changed: the product

| Was | Is |
|---|---|
| binary `agg` | `rlt` |
| Rust package and library `agg` (`use agg::…`), `Agg`, `AggConfig` | `relentless` (`use relentless::…`), `Relentless`, `RelentlessConfig` |
| variables `AGG_<X>` | `RLT_<X>` |
| `<project>/agg/`, `agg.yaml`, `AGG.md` | `<project>/rlt/`, `rlt.yaml`, `RLT.md` (`state/`, `private/`, `roles/` keep their names) |
| `~/.agg/` | `~/.rlt/` |
| branches `agg/<project>/session-N` (and `…/orphaned-<ts>/…`) | `rlt/<project>/…` |
| refs `refs/agg/before|base|partial/…` | `refs/rlt/…` |
| git config `branch.<b>.agg-step`, `agg-partial.<ref>.step` | `rlt-step`, `rlt-partial.<ref>.step` |
| files in the git dir `agg-processes`, `agg-bases-marker`, `agg-verdicts/`, `agg-partial.index` | `rlt-processes`, `rlt-bases-marker`, `rlt-verdicts/`, `rlt-partial.index` |
| commit subjects `agg: session N`, `agg: merge session #N` | `rlt: session N`, `rlt: merge session #N` |
| the process file's key `agg:` | `rlt:` |
| skills `agg-new`, `agg-status`, `agg-supervise` (`/agg:new`) | `rlt-new`, `rlt-status`, `rlt-supervise` (`/rlt:new`) |
| marketplace `agenticgogo`, plugin `agg` | marketplace `relentless`, plugin `rlt` |
| images `agg-claude`, `agg-codex`, `agg-swe-metrics` | `rlt-claude`, `rlt-codex`, `rlt-swe-metrics` |
| release assets `agg-<target>` | `rlt-<target>` |
| repository `ssenge/AgenticGoGo` | `ssenge/relentless` (GitHub redirects the old URL) |

### Changed: judges are verifiers

| Was | Is |
|---|---|
| `judge:` (the backend for rubric verifiers), `judges:`, `steps.<n>.skip_judges` | `verifier:`, `verifiers:`, `skip_verifiers` |
| `any_judge_error` | `any_verifier_error` |
| `agg.judge_timeout`, `run.judge_cache` in a process file | `rlt.verifier_timeout`, `run.verifier_cache` |
| `{ judge: <name>, when: last }` (checks, hooks) | `{ verifier: <name>, when: last }` |
| `dirs.reviews` default `judges` | `verifiers` |
| the folders `agg/judges/`, `~/.agg/judges/`, `plugin/judges/` | `rlt/verifiers/`, `~/.rlt/verifiers/`, `plugin/verifiers/` |
| `agg judge <name>` | `rlt verify <name>` |
| `AGG_JUDGE`, `AGG_JUDGE_SCRATCH`, `AGG_KEEP_JUDGE_SCRATCH` | `RLT_VERIFIER`, `RLT_VERIFIER_SCRATCH`, `RLT_KEEP_VERIFIER_SCRATCH` |
| scenarios: `kind: judge` | `kind: verifier` |
| `calls.jsonl`: `{"kind":"judge", …, "judge":<name>}` | `{"kind":"verify", …, "verifier":<name>}` |
| `verdicts.jsonl`, `gate.json`: the field `judge` | `verifier` |
| `state.json`: `judges[]`, `judged`, `judge_kind` | `verifiers[]`, `verified`, `verifier_kind` |
| `report.json` schema 3 (`judges`, `agg_version`) | schema 4, [`docs/schemas/report.v4.json`](docs/schemas/report.v4.json) (`verifiers`, `rlt_version`); older schemas stay as history |
| `status --json` schema 1 | schema 2, [`docs/schemas/status.v2.json`](docs/schemas/status.v2.json) |
| the web API `/api/reviews` rows `{step, judge, …}` | `{step, verifier, …}` |

`done_if`, `abort_if` and `notify_if` keep their names.

### Added

- **`scripts/readme_check.sh`** runs the README's marked example in a fresh clone; **`examples/hello/`** is a first process
  (an agent step with a script verifier and a rubric review, then a checkpoint) that runs without tokens under a scenario.
- **`scripts/install.sh`** names an `agg` still on `PATH`: "the old agg is still at <path>: remove it once no project needs it".
- A new logo, `scripts/render_assets.sh` (the composed assets) and a favicon for the web UI.

### Changed

- **The README and the docs map** are rewritten: motivation, a first workflow, install, verifiers, then the rest;
  `docs/README.md` is the index and `docs/CONCEPTS.md` holds the ideas. `docs/CONCEPT.md` (an outline) is removed.
- **`examples/hello/` is now the process example;** the old smallest-loop example is `examples/hello-loop/`.

### Fixed

- **No test can hang.** One hung for nine hours on 2026-11-24: a helper that waited for an ask panicked in its own thread
  and left the run under test waiting for ever. Spawns of the binary now have a 900 s bound and end their process group;
  the ask helpers wait 600 s, then end the test binary failed (exit 101). New tests in `tests/watchdog.rs`.

## 0.0.51 (2026-11-25)

### Added

- **`agg: { user_env: true | false | [keys] }`** in the process file, and `Agg::user_env(..)` in a driver: the `env` block of the
  operator's user-scope Claude settings (`$CLAUDE_CONFIG_DIR/settings.json`, else `~/.claude/settings.json`) reaches every
  Claude worker and one-shot call without the plugins, hooks and MCP servers of the same file. Until now a run with
  `user_settings: false` (the default of `agg process`) never read the file, so a model gateway, telemetry or cache switch set
  only there was missing and every session failed with "Invalid API key". Default `false` (nothing); `true` takes every
  string key, a list the named ones; a variable agg's own environment has wins, `AGG_*` keys are never taken, a missing file
  or `env` block is nothing, and only key names are logged or recorded. Codex and Copilot: no-op for now. `agg run` still loads
  the user's settings in full.
- **A reachability preflight in `agg process`** (`agg: { preflight: true }`, the default; `Agg::reachable`): before the first
  step of a fresh or re-entered run, one call ("Reply with the single word ok.") with the worker's agent, model, setting
  sources and environment. A failure stops the run before any step with the agent's error text and, for an authentication
  error where the operator's settings `env` holds `ANTHROPIC_*` keys the run does not pass, the hint to set `user_env`. A call
  that passes is one entry (kind `preflight`) of the run record. Not made for `--resume`, `--check`, `--list`, `--overview`,
  `--brief`, `--report` or a scenario.
- **`session` in the run report**: `{ agg_version, setting_sources: "project,local" | "all", user_env: [key names] }`, recorded
  when the run starts (and as a line of `agg/private/run.log`). Report schema 3
  ([`docs/schemas/report.v3.json`](docs/schemas/report.v3.json)); v2 is superseded and removes nothing.
- **`scripts/install.sh` upgrades in place and warns about shadowed copies.** Without `AGG_INSTALL_DIR` it installs over the
  `agg` first on `PATH` when its directory is writable, else as before. Afterwards it lists every other `agg` on `PATH` with
  its version, prints a WARNING with the exact `cp` and `rm` for each one older than the copy just installed (it never changes
  another copy) and says which copy `agg` resolves to. `AGG_DOWNLOAD_URL=<url>` downloads from another URL (`file://` works).

## 0.0.50 (2026-11-23)

### Added

- **`task_force.checks: [commands]`** in the process file: after the task force answers `fixed` and the failed step is
  met again, these commands run like a step's checks, with `AGG_STEP` (the failed step's id), `AGG_STEP_KEY`,
  `AGG_STEP_LABEL`, `AGG_PROCESS_HOME` and `AGG_FIX_BASE` (the commit before the repair; across attempts at one failure,
  the commit before the first). An unmet verdict means the repair is not accepted: the attempt counts against
  `task_force.max`, and the verdict goes into the next attempt's brief. `--check` validates the commands. Embedders:
  `Flow::fix_checks`.
- **A stopped session's work in the next session's brief.** When an agent step starts or is re-entered and a partial ref
  or an orphaned session branch holds work of a session of the same step, the brief gets a section "Work a stopped
  session left": the ref, `git diff --stat` against its base and how to take what holds (at most the three newest refs,
  40 lines of stat each). agg never merges it itself. To tie a session to its step, agg now notes the step's name in the
  repository's config when it cuts a session branch (`branch.<name>.agg-step`, which follows the branch when it is
  parked) and when it saves a partial ref (`agg-partial.<ref>.step`); refs made before this carry no step and are not
  offered.

## 0.0.49 (2026-11-15)

### Changed

- **The judges' scratch is removed when the run ends** (`$AGG_JUDGE_SCRATCH`, every session's, on the host and in the
  container dir). What a verdict names as `evidence` was copied to `agg/private/evidence/` when it was judged; anything
  else a judge left there (a copied tree, rendered deployment files that can hold secrets) no longer stays readable on
  the host. The toolchain cache is kept. `AGG_KEEP_JUDGE_SCRATCH=1` keeps the scratch, for debugging a judge.

## 0.0.48 (2026-11-15)

### Added

- The `/process` page shows a running process live: what it does now (step, agent and model, the current activity or
  the checkpoint it waits at, how long it has been quiet), the session's activity, and the critics' last verdicts
  (`GET /api/reviews`).

### Fixed

- **`--resume` after a re-entry**: a run stopped after a re-entry (a checkpoint decision's or the task force's) resumed
  from the first step against a ledger that starts at the re-entered step, and stopped with `resume DIVERGED`. A start
  now records its arguments (`agg/private/started.json`) and a plain `--resume` continues that run with them; a step the
  ledger recorded is replayed, not decided again (a refresh whose inputs look unchanged now ran a session then).

## 0.0.47 (2026-11-14)

### Added (A-39, the web UI of a process)

- **`agg serve --projects <folder> --process <file>`**: every endpoint takes `?p=<project>`; `POST /api/run`
  (`start`, `resume`, `stop`), `GET /api/checkpoint` (the checkpoints waiting for a human with their document, open points
  and decisions), `POST /api/answer` takes `decisions` (appended to the checkpoint's decisions file before the answer),
  `GET /api/projects`, `GET /api/ui`, `POST /api/projects` (a new project from the process's form, then its run).
- **`ui:` in the process file**: `new_project` (a command and its form fields) and `overlay` (a command whose facts agg
  merges into each step's `extra` and the report's `extra`); `--check` validates the fields.
- The web UI's `/process` page: projects, the new-project form, Start fresh · Resume · Stop, the checkpoints to answer
  with a decision, a step's facts on the map. `scripts/web_process.py` checks it in a real browser (`scripts/e2e.sh`).

### Fixed

- `run.commit_inputs` no longer commits agg's own runtime (`agg/private/`) when the project has no `.gitignore` yet.

### Changed

- **An LLM judge's verdict is enforced by the agent**: the ruler gets the verdict's JSON Schema (Claude `--json-schema`,
  Codex `--output-schema`), so a judge can no longer answer in prose and fail with "did not emit valid verdict JSON",
  which cost a repair round. Copilot has no such flag and keeps the tolerant parse. `Backend::one_shot` takes a
  `schema: Option<&str>` (`docs/BACKENDS.md`).

## 0.0.46 (2026-11-14)

### Added

- **`agg process <file> --overview [--columns <key>,…] [--only-with <key>]`**: a Markdown table per phase from the steps'
  `meta` fields (`docs/PROCESS.md`): a row per numbered step with a `meta`, its label first, a column per key (all keys in
  order of first appearance, or the ones `--columns` names); `--only-with <key>` keeps the steps that have that key.

### Changed

- The sources no longer name a consumer of the process runner; the history was rewritten to match (tags and commit
  hashes changed).

## 0.0.45 (2026-11-14)

A-37: process files. A process of standard steps runs from YAML.

### Added

- **`agg process <file> <project>`** (`agg::workflow::process`, `docs/PROCESS.md`, `examples/process/`): phases of steps,
  each a `script`, an `agent` (checks, a critic, a ladder), a `checkpoint`, an `each` over a list in a file, an `exam`, or
  `outside`/`planned` (numbered, not run). It builds each entry into the block agg already has; the YAML has no
  logic of its own. A step is its **id**; numbers and checkpoint names (`CP<k>`) are computed from position, so
  rearranging a process is an edit of its file. `--check` validates the file and everything it names (a verdict JSON,
  exit 1), `--list` prints the numbered steps, `--from`/`--until` take an id, a number or an instance key.
- Ported from a field driver and generalized: layered step settings (`worker`, `critic`, `repairs`, `inputs`, `outputs`,
  `first`, `ladder`, `note`) with a project's overlay file; ladders (tool tiers, a decider); the exam kind; checkpoints with
  decisions files, open points and auto-approval; a step's base (`refs/agg/base/<key>`); hooks around steps and phases;
  ending processes a step left running; the critic's rerun context; the brief's small input files.
- **`agg cached [--ignore <path>]… -- <command>…`** (hidden): a check's met verdict answered from an earlier run on exactly
  the same project content; the process wraps script checks in it.
- `Flow::keys`, `Flow::name_key`, `Flow::key_of`: a step may have an explicit key (its first word stays the default), so
  history, `refs/agg/before/<key>`, the task force's count and `--from` go by it. History entries carry the key as `id`.
- `{{ step.build }}`-style names: a template name with dots and `-` is filled in when the variables have it (a project's own
  `{{ a.b }}` still stays as it is).

### Added (A-38, with the process file)

- **Command judges**: `judges.<name>: { command: "…", path: [dirs] }` in `agg.yaml` and in a process file: a shell command held
  to its exit code, its last lines as the rationale (`JudgeKind::Command`, `Judge::command`, `CommandJudge`; `docs/CONFIG.md`).
- **Declared judges in process files**: a top-level `judges:` (the same declarations as `agg.yaml`'s) named by `checks: [{ judge: <name> }]`
  and by `{ judge: <name> }` entries of the hooks files; `--check` refuses an undeclared name and a declaration of no kind.
- **`run.declared`**: before a step, its declared inputs that an earlier step wrote exist; after it, its declared outputs.
- **`run.scripts_runnable`**: scripts a step wrote under the folders can run (`#!` files executable, shell scripts parse).
- **`run.commit_inputs`**: the project's inputs are committed on a branch before the run, and the run's first record entry
  names the process commit that ran.

- **Scenario mode** (`agg process <file> --scenario <files>… --projects <dir> [--keep]`, hidden `agg scripted-agent`,
  `docs/PROCESS.md` "Scenarios"): the process runs on a copy of a project with scripted agents and scripted answers to
  asks; each scenario's `expect` (record, never, sessions, files, exit) is checked. `AGG_SCENARIO`, `AGG_SCENARIO_LOG`,
  `AGG_SCENARIO_PROMPT` are set for a rule's commands. New dependency: `regex`. Unix only.

- **Run report** (`agg process <file> <project> --report map|timeline|profile [--json]`, `workflow::process::report` and `profile`,
  `docs/PROCESS.md` "The run report"): where the run is, when each step ended, where the time went. `agg serve`:
  `GET /api/process?report=…`; the web UI: a `/process` page; `agg status` shows the running process step. `agg process`
  notes its file in `agg/private/process.json`. `agg status --json` is unchanged (its schema is strict; the step is in `current_step`).
- `--report … --since <epoch> | --current <step> | --stopped | --log <driver log>`; `agg cached --content` (the content hash).
- Scenario patterns may use look-around (`fancy-regex`).
- **Standing texts** (`workflow::process::standing`, `defaults/`, `docs/PROCESS.md`): agg's default `run.system_prompt`, `task_force.prompt`,
  `run.critic_scope`; `{ append: <file> }` after a default; `run.notes: <file>` (a brief section and a check of the section's length);
  `run.brief.shared_machine`; the quality guide's `{table}` from `swe-metrics score --describe`; `agg process <file> <project> --brief <step>`.
  A process that set none of these now gets the defaults (the system prompt and the critic's scope).
- **The `template` judge** (`judges.<name>.template: { doc, from, placeholder? }`, `workflow::process::form`): a document filled in from a template.

### Changed

- `Flow::reenter` records the re-entry under the step's name (its label in a process), not its bare key.

## 0.0.44 (2026-11-11)

From an unattended field run of a driver.

### Changed

- **A driver's run ends on the base branch** (`Agg`'s drop, `git::back_to_base`): after a halt the repo stayed on
  the halted session's branch, a human's commit landed there, and the next start parked it as
  `orphaned-<ts>`, so the base branch never got it.
- **A spent task-force limit exits 6** (`agg::workflow::task_force::SPENT_EXIT`) and adds a run-record entry
  starting `spent`, so an operator tells it from an escalation (3) without reading the halt line.
- **The unchanged-note message names `--decided`**: a note the step needn't act on goes there instead.

## 0.0.43 (2026-11-10)

From two parallel unattended field runs of a driver.

### Changed

- **The task force's limit holds per failed step** (`agg::workflow`, the task force; `agg/private/task-force.json`,
  its new `steps` map): one limit for the whole run was spent on early repairs, and every later failure halted
  unrepaired until an operator restarted the run. Each failed step now has its own `max` re-entries and repairs;
  the count is kept across the driver's processes, and a run from the start begins it anew. The halt names the
  step whose limit is spent: `(the task force's 3 re-entries and repairs for <step> are spent)`.

## 0.0.42 (2026-11-09)

From the first unattended field run of a driver (two increments, 11 manual restarts).

### Fixed

- **A judge's prompt goes to the agent on stdin** (`backend::claude`, `backend::codex`, `os::proc::run_with_input`):
  it was one argument, and Linux caps one argument at 128 KiB (`MAX_ARG_STRLEN`) whatever `ARG_MAX` says. Every
  review over that failed with `spawn: Argument list too long (os error 7)`. Claude gets `-p` with no argument,
  Codex `exec … -`; stdin is closed after the prompt. A container judge runs with `-i`.
- **A judge that fails to run is asked once more, and then grades nothing** (`Agg::judge`, `agg::workflow`): its
  error stood after one try, and an agent step took the critic's error for a point to keep for the next approval
  and went on. The step is now not graded: no repair round (a worker can't fix a judge), never met; it goes to
  the task force, or ends the run.
- **A repair in place that the re-judge still finds unmet goes to the task force again** (`agg::workflow`, the task
  force): it ended the run, and a plain restart then usually passed. The task force gets the new failure while
  its limit lasts.
- **The task force's limit holds for the run, not for one process** (`Flow::new`; `agg/private/task-force.json`):
  a re-entry or an operator's restart `--from` a step started at the `--reentries` it was given, and the repairs
  in place at 0. The count is kept between the processes; a run from the start begins it anew.
- **A halt's reason is the driver's last line** (`Flow::halt`), on stdout and stderr: `halt: <step>: <why>`. A log
  ended with only the exit code.
- **`--until` ends the run before the first step after it in the driver's order** (`Flow::order`): with the steps up
  to it not built, the check never matched, and the next step ran.
- **What a driver writes between two steps is no protected-path violation** (`Agg::step`): the check of a
  change between sessions compared a step's session against the last one of a step with the same `write_deny`,
  and put back what the driver had written in between (its history), so a second task-force round lost the
  first round's record. A driver's new step starts from the world the driver left; between the sessions of one
  step the check holds.
- **The Linux release binary runs on glibc 2.17 and later** (`release.yml`, built by `cargo zigbuild`): it needed
  the runner's glibc, and failed on Amazon Linux 2023 (2.34).

### Added

- **`Flow::constraints`**: what a failed step must keep to (its scope, its rubric), as the driver says, in the
  task force's brief, so that a repair in place doesn't fail on another of the step's rules.
- **`--note-file <file>`** (`Run::from_args`): a driver's note from a file, which no shell has to quote; a
  re-entry replaces it like `--note`.
- **`agg doctor` warns when a Codex judge would miss your provider**: `model_provider` in your Codex config, but
  not in `AGG_CODEX_CONFIG`. `docs/CONFIG.md` shows how to carry a provider of your own there.

## 0.0.41 (2026-11-03)

### Added

- **A step whose instructions changed since its result is told how** (`agg::workflow`, `Flow::task`; SR-P3):
  agg keeps what each step was told for the result it gave (`agg/private/instructions/<step>.md`). A later call
  whose brief differs gets the diff in a section of its own, "Your instructions changed since your result", and
  "Bring your result in line with what changed, and change nothing else". Such a step is never untouched, even
  where none of its inputs changed. A `--reset` run compares nothing. Before, a `--from` after a prompt change
  handed the worker its whole task beside a result that looked done, and it kept the result as it was.

### Fixed

- **A new run withdraws the open asks of the run before** (`core::asks`, `agg answer`; SR-P6): the ledger gets
  a `withdrawn` row with the reason for each of the driver's asks still open when a run starts ("the run before
  <run id> ended"), and the run's log says how many. A withdrawn ask is no longer open, and `agg answer` refuses
  it: nothing would read the answer.
- **Only the task force's re-entries count against its limit** (`agg::workflow`, the task force's `rerun`;
  SR-P7): a re-entry for a checkpoint's decision (`--decided`) kept adding to `reentries`, so a run whose
  approvals carried decisions halted at its first fault after them without the task force. It now keeps the
  count.

## 0.0.40 (2026-11-02)

### Fixed

- **A run that reaches its `--until` without having come to its `--from` halts** (`agg::workflow`,
  `Flow::runs`): it skipped every step up to `--until` and ended there as done, exit 0, having run nothing. A
  `--from` no step has (a typo, a step the driver doesn't build) passed that way. It now ends `Halt`, with
  "--from <step>: the run came to no step <step> before its --until <step>".

## 0.0.39 (2026-10-30)

### Fixed

- **A step called again after its result in the same run is a new task** (`agg::workflow`, `Flow::task`): a
  re-entered run took every later call of a step for a refresh of its last result, so a loop's step called once
  per failure got "Since your last result … change nothing" and the `--from` note again, and changed nothing.
  The history now knows which steps this run gave a result of (`done`, `unchanged`); a call after one gets the
  whole task. A ladder's next tier after an attempt that ended unmet still refreshes and keeps the note.
- **A new run keeps the previous run's call log** (`agg/private/calls.jsonl`): a fresh start sets it aside as
  `calls.<unix seconds>.jsonl` instead of deleting it, and a resume keeps a copy of the calls it drops beside the
  rewritten ledger. Every launch's sessions, verdicts and merges stay on record for a run's profile.

## 0.0.38 (2026-10-29)

### Added

- **Agents converge on a task before it is carried out** (A-36, `agg::workflow`): `Task::voices(roles)` has
  agents on other roles discuss the step's task before the worker's session. Each takes a position on its own
  first, then in rounds each in turn reads the others' latest positions, agrees or disagrees with evidence,
  changes a point only for evidence, and ends with `DISAGREE: <n>`, the others' points it still disputes; the
  discussion ends when all agree, or after three rounds. The voices change no
  file of the project (their positions are in the run's scratch folder); the worker gets the last positions in
  its brief, carries out what they agree on and names each point it decided where they don't. The history
  records `discussion` (met when the voices agreed); a refresh doesn't discuss again, and a voice that writes no
  position fails the step.

## 0.0.37 (2026-10-27)

### Added

- **The task force repairs in place** (A-35, `agg::workflow`; formerly triage): when a step fails, the
  task force's answer has an `outcome`, `fixed`, `reenter` or `escalate`. `fixed`: it repaired the cause
  itself, and the failed step is judged again (an agent step by its checks and critic, a script step by its
  script, a failed hook by the hook); met, the step is done and the run goes on, recorded `task-force` (was
  `triage`) with the files it changed. Going back to an earlier step, which refreshes every step after it,
  stays the answer for an earlier result that is wrong. An answer without `outcome` reads as before; its file
  is `task-force.yaml`. `Flow::recover(name, why, again)` gives a driver's own failure the same repair.
- **A decision is no fault** (`Run::decided`, `--decided`): `Flow::reenter` re-enters with the humans'
  decision, and the step it goes back to refreshes against it like a later step: it sees the decision as a
  change to what it reads, and a result the decision doesn't concern stays unchanged, where before a session
  that changed nothing failed as a note it hadn't carried out.
- **`blocked_by` in a verdict:** a check unmet because of another step's result says so; the step gets no
  repair round for it and goes to the task force at once.

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

- **Workflow building blocks** (A-26 to A-31, `agg::workflow`), moved from a caller's driver where they ran fifteen
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
