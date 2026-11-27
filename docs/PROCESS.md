# Process files — `rlt process`

A **process** is phases of steps. Each step is one of a closed set of kinds: a script, an agent's work verified by checks
and a critic, a human's approval, a loop over a list, an exam. `rlt process <file> <project>` runs it from YAML. It is the
YAML twin of the Rust driver blocks in [`relentless::workflow`](RUST_API.md#workflow-building-blocks-aggworkflow): every entry
becomes a call of the block that already exists. **The YAML has no logic of its own** — no expressions, no `if:`, no
variables beyond the ones listed here. A condition is a script's verdict; real logic is a script or a Rust driver.

Reach for it when the flow is a *process* (steps in order, each verified, humans at checkpoints) rather than one loop of
sessions on a goal (that is `rlt.yaml`, [CONFIG.md](CONFIG.md)). The two are separate: `rlt.yaml` is unchanged.

```
rlt process <file> <project> [--resume] [--from <step>] [--note <text> | --note-file <f> [--decided]]
                             [--reset] [--until <step>] [--reentries <n>]
rlt process <file> --check     # validate the file; a verdict JSON last, exit 1 on any problem
rlt process <file> --list      # the numbered steps as JSON
rlt process <file> --overview [--columns desc,done] [--only-with done]   # a Markdown table per phase from the steps' `meta`
```

`examples/process/` is a complete, commented example (it is also what the test suite runs).

## Identity, numbers and names

A step is **its id** (`lower-case-with-dashes`) everywhere: the history, `refs/rlt/before/<key>`, the task force's count,
`--from`, `skip`, settings. **Numbers are computed from position**, never written: a phase's number is its index, a step's
`<phase>.<k>` (every kind counts; an `each`'s children are numbered in its place), a checkpoint's name `CP<k>` by its place
among the checkpoints. Reordering, adding or removing a phase or step changes numbers and nothing else: prompts and
scripts name other steps as `{{ step.<id> }}` (`3.4 As-is architecture`) and checkpoints as `{{ cp.<id> }}` (`CP2`).

A step's **label** is `<number> <name>`; a checkpoint's `<number> CP<k> <name>`; an instance of an `each`'s child
`<number>-<item> <name>` with the key `<id>-<item>`. The run record's entries carry `step` (the label) and `id` (the key).

`--from` / `--until` take an id, a number (`3.4`), an instance key (`build-U1`) or an instance's number (`2.1-U1`); anything
else names the closest ids. `--from` a step the run never comes to halts it.

## The file

Every key but `version` and `phases` is optional; an omitted feature is off. Unknown keys are errors.

```yaml
version: 1
vars: { app: app }                    # {{ name }} everywhere; values may name {{ home }}, {{ env.NAME }}
vars_file: layout.yaml                # more variables from a YAML map (vars wins)
roles: { file: tools.yaml }           # in the project: worker and critic (agent, model, effort)
dirs: { prompts: prompts, reviews: verifiers }
settings:
  project: settings.yaml              # the project's overlay file (in the project)
  extra_keys: [other]                 # keys of that file rlt tolerates
  defaults: { repairs: 1 }            # per-step settings, see below
  skip: [extras]                      # ids of phases or steps kept in the order, not run
  quality: { definition: d, version: "1", min: 90, guide: prompts/quality.md, reports: verifiers/reports.py, scope: "{{ app }}" }
  models: { small: some-model }       # sizes a ladder's tier may name
rlt: { memory: false, user_settings: false, user_env: false, preflight: true, on_regression: annotate, verifier_timeout: 3600, min_free_disk: 10GB }
run:
  history: reports/steps.jsonl        # the step history (default rlt/private/steps.jsonl)
  own: [task.md]                      # files --reset never takes back
  start: [scripts/start.sh]           # before rlt opens the project; the last stdout line is logged
  config: run.yaml                    # `switched_off: { <id>: <why> }`, written by a step
  bases_reset_when_changed: increment.yaml
  system_prompt: prompts/system.md
  brief:
    sections: [{ file: prompts/house-rules.md }]   # in every agent's brief; {{ this }}, {{ this.id }} …
    inputs: { always: [notes.md], max_file: 16000, max_all: 48000 }
  critic_scope: prompts/critic-scope.md            # {{ outputs }} is the step's outputs
  open_points: checkpoints/open-points.md
  leftovers: true                     # end processes a step left running
  verifier_cache: { ignore: [notes.md] }
  hooks: { process: hooks.yaml, project: hooks.yaml }
  auto_note: scripts/waiting.sh       # stdout appended to an auto-approval's record
  commit_inputs: { branch: main, message: "Inputs at run start" }   # commit what the project folder holds, before the run
  declared: true                      # check the inputs and outputs steps declare (below)
  scripts_runnable: [scripts-out]     # scripts a step wrote under these folders can run (below)
verifiers:                               # declared verifiers (below), named by `checks: [{ verifier: <name> }]` and hooks entries
  tests: { command: "pytest -q", path: [.venv/bin] }
  parse: { metric: { id: syntax.parse_errors, scope: docs }, max: 0 }
deny: { inputs: [task.md] }           # named write-deny lists
task_force:
  prompt: prompts/task-force.md
  max: 3                              # re-entries and repairs per failed step
  deny: [inputs]
  sections: [{ text: "…" }]
  trust: [{ failed_in: build, reenter: plan, except: [deploy] }]   # proposals that go ahead without a human
  checks: [scripts/fix-check.sh]      # a repair in place must pass these (below)
ladder: { tool_run: "scripts/tool.sh {{ item }} {tool}", tool_commits: ["{{ app }}"], tools: [codemod] }
phases: [ … ]
```

### Phases and steps

```yaml
phases:
- id: build
  name: Build
  pre: [scripts/index.sh]             # when the run enters the phase
  post: [scripts/seal.sh]             # when it leaves it
  steps: [ … ]
```

Every step has `id`, `name`, `kind` and optionally:

| key | meaning |
|---|---|
| `when` | a command, or commands that must all be met (the first unmet one decides); unmet: the step is skipped, recorded with the reason. A step before `--from` counts as applying |
| `deny` | write-deny entries: a name of the top-level `deny:` or a glob |
| `pre`, `post` | commands before the step and after it is done (neither for a skipped step or one before `--from`); a failure fails the step |
| `settings` | the step's defaults (below) |
| `constraints` | text for the task force's brief when this step fails, beside the step's rubric |
| `meta` | free-form, ignored by rlt, handed through by `--list` (descriptions for docs and slides) and shown by `--overview`: a row per step with a `meta`, a column per key (`--columns` chooses and orders them; `--only-with <key>` keeps the steps that have it) |

### Kinds

| kind | what runs | keys |
|---|---|---|
| `script` | a script step: it does the work and gives the verdict | `run` |
| `agent` | an agent's task, its checks, its critic, through its ladder | `prompt` (default `<dirs.prompts>/<id>.md`), `checks`, `review` (`true` = `<dirs.reviews>/<id>-review.md`, or a path), `repairs` (default 2), `sections`, `quality`, `on_fail` |
| `checkpoint` | a human approves a document | `prepare`, `doc`, `question`, `owner`, `go_live` |
| `each` | its child steps once per item of a file | `items: { file, list, items, key }`, `steps` |
| `exam` | an examinee, checks, a grader, a repairer, in rounds | see below |
| `outside` | nothing: a step done outside the run, numbered and listed | — |
| `planned` | nothing: a step not built yet, numbered, in the order (`--until` may name it) | — |

**Commands** are shell commands in the project; a first word with a `/` is a path relative to the process home. Every
script gets `RLT_STEP` (the key), `RLT_STEP_LABEL`, `RLT_PROCESS_HOME` and, in an `each`, `RLT_ITEM`. A check prints a
verdict JSON as its last line. A check is a command, or `{ run, name, when: last }` (`last`: only the last item's instance
of an `each` runs it). Check verifiers are named after the step and the command unless `name` is given; every script check
goes through the verifier cache.

**Sections** (`sections:`) are `{ text }`, `{ file }` (read and rendered) or `{ run }` (a command's output, run when the
brief is built). A step's own sections are appended to its task (the critic sees them); the run's `brief.sections`, the
small input files and the project's note are the worker's only.

**`agent`** — an agent step in order: a refresh whose `settings.inputs` didn't change is `unchanged` without a session;
`settings.first` does what it can; the worker's brief gets what is new first, then the task, the sections, the critic's
rubric and the checks; the checks, then the critic. A critic's point left after its round is kept for the next
checkpoint (`run.open_points`). `quality: true` adds rlt's score verifier on the files changed since the step's base. `on_fail`
runs when the step ends the run unmet, before the error goes on (a deployment it left up is taken down).

**`checkpoint`** — `prepare` writes `doc`; `question` asks about it. The humans write decisions into
`<dir of doc>/<id>-decisions.md` first. A yes with new lines there re-enters `owner` with them as a decided note: the owner
**must read that decisions file** among its `settings.inputs` (`--check` refuses otherwise). A checkpoint in the project's
`auto_approve` (by id, by `CP<k>` name, or `all`, which never covers a `go_live` one) is approved by the run, its record
saying what waits for the humans (`run.auto_note`).

**`each`** — `items.file` (in the project) holds a list of maps (`items`, default `units`) each with a `key` field
(default `id`); `list` names the key holding the order. An item's fields are `{{ item.<field> }}`, the item as YAML
`{{ item.yaml }}`; `{{ before }}` and `{{ base }}` are the instance's refs. A run that starts after the loop, in a project
without the file yet, skips it.

**`exam`** — `checks` run first each round; `examinee` (`prompt`, `sections`, `deny`) answers on a fresh copy of what it
may read; `answers` is a check of its result; `grader` is a rubric; `repairer` fixes what the round found (and gets the
checks to run and a disputes file); `fresh` lists files removed before each round; `findings_ids: { file, lists }` tells
rounds apart by the ids a verdict names (the same set twice is a stall: the step fails); `rounds` (default 5);
`dispute: { reenter: <id> }` sends the run to that step once when the repairer disputes a finding; a second dispute fails.

### Settings

Per step, highest first: the project's file (`steps.<id>`, then `defaults`), the step's `settings`, the process's
`settings.defaults`. Fields: `worker {agent, model, effort}`, `critic` (`false`, `true` or a role), `repairs`, `inputs`,
`outputs`, `tools`, `first` (a command), `ladder`, `note`, `retry` (a policy for a worker session that failed for no
fault of the work, or `false`; [Retries and timeouts](CONFIG.md#retries-and-timeouts)) and `timeout` (the longest one try may run,
e.g. `45m`). The project's file also has `skip` (replacing the process's),
`quality`, `models`, `managed` (tools a ladder's decider may pick besides `ladder.tools`) and `auto_approve`. Unknown keys
are errors; every id and name it uses must exist in the process.

A **ladder** is the step's tiers, first to last: `{ mode: once | dcr | dccr | converge, model, effort, agent, repairs, max }`,
`{ tool: <name> }` (runs `ladder.tool_run`; a verdict with `value: 0` means no tool applied: recorded `n/a`) or `agent`
(a cheap agent picks the next tier). The first tier that meets the step's checks and critic ends it; the last fails into
the task force. A re-entry with a note starts one tier higher.

### The task force, hooks, the cache

A failed step goes to the task force (`task_force`): an agent that finds the cause, repairs it in place or proposes a
re-entry (`reenter`, an id) or escalates. The limit holds per failed step and survives restarts; a spent limit exits **6**.
`trust` rules let a proposal go ahead without a human. **`checks`** are commands run like a step's checks (in the project, a
verdict JSON as the last line) after a repair in place that met the failed step again; they get `RLT_STEP` (the failed
step's id), `RLT_STEP_KEY`, `RLT_STEP_LABEL`, `RLT_PROCESS_HOME` and `RLT_FIX_BASE` (the commit before the repair; across
attempts at one failure, the commit before the first). An unmet verdict means the repair is not accepted: the attempt
counts against `max`, the verdict is recorded (kind `hook`, `task force check <name>: …`) and goes into the next
attempt's brief as the failure. The change stays in the tree for the next attempt to correct. A hooks file has `step:`, `steps: { <id>: … }` and
`phases: { <id>: … }`, each `before`/`after`: scripts that run around every step or phase and print a verdict; the project's
file adds to the process's.

`rlt cached [--ignore <path>]… -- <command>…` (`--content` prints the project's content hash instead, for a caller that keeps its own results by content; the process wraps script checks in it) answers a met verdict from an earlier
run of the same check on exactly the same project content; unmet verdicts are never cached.

### A stopped session's work

A session cut short (a rate limit, an interrupt, a crash) is never verified or merged, but its work is kept: on a partial ref
(`refs/rlt/partial/<run>/<session>`), or on its session branch, which the next run parks as
`<prefix>/<project>/orphaned-<ts>/session-<N>`. rlt notes which step a session belonged to when it cuts the branch or saves
the ref (the step's name, the key a `--from` takes) in the repository's config (`branch.<name>.rlt-step`,
`rlt-partial.<ref>.step`), since the ledger records a step only once it has ended. When an agent step starts, or is
re-entered, and such a ref holds work of a session of the same step, its brief gets a section **`## Work a stopped session
left`**: the ref, the commit it stands on, `git diff --stat` against it (at most 40 lines), and how to take what holds
(`git diff <base> <ref>`, `git show <ref>:<path>`, `git checkout <ref> -- <path>`). At most the three newest refs are named;
a ref whose files HEAD already has as the session left them is not. rlt never merges it itself; a session branch or
ref made before rlt noted the step is not found.

### What every run needs (A-38)

- **Declared verifiers** (`verifiers:`): a check or a hooks entry names one as `{ verifier: <name> }`, next to plain commands
  (`checks: [scripts/check.sh, { verifier: tests }]`, in a hooks file `after: [{ verifier: parse }]`). A declaration is one of
  `rlt.yaml`'s (`docs/CONFIG.md`): a `metric:` or `score:` over swe-metrics with its `max:`/`min:`, or a **`command:`**, a shell
  command run in the project and met when it exits 0 (the last four non-blank lines of its output, at most 400
  characters, are the rationale; `path:` entries go in front of `PATH`, relative to the project). `--check` refuses a
  `verifier:` naming no declaration and a declaration of no kind.
- **Declared inputs and outputs** (`run.declared: true`): before a step, every input it declares (`settings.inputs`) that an
  earlier step of this run (a met `done`, `tool` or `unchanged` entry of the record) declares as an output exists; after it,
  every output it declares exists. Globs and paths with names only a step knows (`{{ item }}`) are left out, an instance of
  an `each`'s child takes its child's declaration. The verdicts are `declared-inputs` and `declared-outputs`, recorded as
  hooks are.
- **Scripts that can run** (`run.scripts_runnable: [<folder>, …]`): after a step, every file it changed or added under
  those folders (since `refs/rlt/before/<key>`; all of them without a mark) that starts with `#!` is executable, and every
  shell script (`.sh`, or a `#!` line ending in `sh` or `bash`) passes `sh -n` (`bash -n`).
- **Inputs committed** (`run.commit_inputs`): before rlt opens the project, the project is switched to `branch` (a failure
  stops the run, exit 1, with the reason, as a failing `run.start` command does) and everything its folder holds is
  committed with `message`, unless nothing changed; once more after the `run.start` commands. The run's first record entry
  (kind `start`) says which process ran: `process commit <HEAD of the process home>` with ` with uncommitted changes` where
  its checkout has some, or `process home not in git`; the line is printed too.
- **History**: `rlt history` lists process runs with their usage like any other.

### The rlt run's own settings (`rlt:`)

| key | default | meaning |
|---|---|---|
| `memory` | `false` | working memory between sessions (rlt's `STATE.md`, wiki, summary) |
| `user_settings` | `false` | the operator's own plugins, hooks and MCP servers in the workers' sessions. Off, Claude workers start with `--setting-sources project,local` and never read the user-scope settings file |
| `user_env` | `false` | the `env` block of that file, without the rest of it: `false` (nothing), `true` (every key) or a list of key names |
| `preflight` | `true` | one tiny call with the worker's agent and model before the first step; `false` leaves it out |
| `on_regression` | `annotate` | `annotate` (record a regression, keep the work) or `rollback` |
| `verifier_timeout` | `3600` | a verifier's time limit, seconds |
| `min_free_disk` | `10GB` | the least free disk the run needs (a size, a share like `5%`, or `false`), checked before each worker session and each round of verifiers; below it the run stops (exit 7) and `--resume` goes on |

**`user_env`.** The user-scope settings file (`$CLAUDE_CONFIG_DIR/settings.json` when that variable is set, else
`~/.claude/settings.json`) holds an `env` object: a model gateway's base URL and token, telemetry and cache switches. A worker
without the user's settings never reads it, and on a host where the gateway is set only there every session fails with
"Invalid API key". `user_env` copies the keys it names (string values only) into the environment of every Claude worker and
one-shot call (verifiers, summaries), and nothing else of the file. A variable rlt's own environment already has wins over the
file (the operator's shell overrides it), the keys rlt manages itself (`RLT_*`) are never taken, and a list naming a key the
file lacks takes nothing for it. A missing file, or one without an `env` block, is nothing and no error. Only key names are
ever logged or recorded, never values. Codex and Copilot workers take nothing from it for now.

**The preflight.** Before the first step of a fresh or re-entered run (not for `--resume`, `--check`, `--list`, `--overview`,
`--brief`, `--report` or a scenario's scripted agents), rlt makes one call, "Reply with the single word ok.", with the
worker's agent, model, setting sources and environment, exactly as a worker's session starts. (Not a verifier's one-shot: a verifier
loads the operator's settings, so it would pass where the worker fails.) If it fails, the run stops with the exit code of any
fatal error before a step starts, naming the agent's error text (its first lines). When that text is an authentication error
("Invalid API key", 401, "authentication") and the operator's settings `env` holds `ANTHROPIC_*` keys the run does not pass, it
adds the hint `the operator's settings env has <KEY NAMES>; set rlt: { user_env: true } (or a list) in the process file`. A
rate limit counts as an answer. A call that passes is one entry (kind `preflight`) of the run record.

**What the run reports.** `rlt/private/report.json` (schema 3) has a `session` object, recorded when the run starts:
`{ rlt_version, setting_sources: "project,local" | "all", user_env: [key names] }`, and `rlt/private/run.log` a line with the
same. Names only.

### Templates

`{{ name }}` in the file's strings and in the files it reads is filled in from: `vars`/`vars_file`, `home`, `project`,
`step.<id>`, `step.<id>.number`, `step.<id>.name`, `cp.<id>`, `phase.<id>` (`phase.<id>.number`, `.name`); in a step's strings also `this`, `this.id`, `this.key`,
`this.name`, `this.number`, and in an `each` `item`, `item.yaml`, `item.<field>`, `before`, `base`; in `vars` values
`env.NAME`. In the file's own strings an unknown name is an error; a prompt's own template language (`{{ a.b }}`) is
left alone unless the name is `step.…`/`cp.…`/`phase.…`, which `--check` verifies (`phase.…` too).

### Standing texts (A-38)

Defaults embedded in the binary apply when the process file sets nothing; a text it names **replaces** the default, and
`{ append: <file> }` follows it:

| key | default | notes |
|---|---|---|
| `run.system_prompt` | the worker's brief is `rlt/private/INSTRUCTIONS.md`: read it first, all of it; the final message is one line | `<file>` replaces, `{ append: <file> }` adds |
| `task_force.checks` | commands that verifier a repair in place (above); none by default | optional |
| `task_force.prompt` | the goal, how to find the cause (the record, `git log`), what it never changes (checks, process, decisions), the answer's form (`fixed`, `reenter <step id>`, `escalate`), a flaky failure | the task force exists where `task_force:` is set |
| `run.critic_scope` | review only this step's outputs (`{{ outputs }}`); never an input, a human's approval, or a requirement nobody stated | |
| `run.notes: <file>` | a brief section: read the notes file first; at the end write your own section `## <step id>` of at most five lines; a check after the step is unmet when it has more (`notes section of <id> has N lines, at most 5`) | no default: off without the key |
| `run.brief.shared_machine: true` | a section: other runs share this machine: your own temp folder, end only processes you started, by their PID | off by default |
| `settings.quality.guide` | the guide file's `{table}` is filled with the score definition's hard limits and parts (`swe-metrics score --describe`), as the `score` verifier scores | with `{min}`, `{definition}`, `{version}`, `{since}`, `{reports}` |

`rlt process <file> <project> --brief <step>` prints the process's parts of what a step's worker and critic get, without
running anything: the system prompt, the worker's prompt, the standing sections, the step's sections, the quality guide, the
critic's scope and rubric (a section's command isn't run); `--brief task-force` prints the task force's prompt.

### The template verifier (A-38)

```yaml
verifiers:
  report-form: { template: { doc: docs/report.md, from: templates/report.md } }   # `placeholder: <regex>` to change `<…>`
```

A document filled in from a template (`doc` in the project, `from` in the process file's folder, both with the process's names
filled in). Met when the document has the template's headings in the template's order (level and text; a placeholder in a
heading matches any text), no section is empty (a parent holding subsections isn't), and nothing of the template is left: its
placeholders (`<…>` outside code, or the declaration's `placeholder` pattern; only those the template has), an HTML comment, a
`> Instruction:` line or a blockquote line the template has. The rationale names the first three problems with their line.
A process file's `verifiers:` only (rlt.yaml refuses it).

### The run report (A-38)

`rlt process <file> <project> --report map|timeline|profile [--json]` (`--since <epoch>`: only the record from then on; `--current <step>` and `--stopped`: what a caller knows better than `state.json`; `--log <driver log>`: the tool calls of the sessions, for the profile) reads what a run left (the record, `rlt/private/calls*.jsonl`,
`asks.jsonl`, `report.json`, `state.json`) and prints it as text, or with `--json` as one JSON document:

- **map**: `{phases: [{n, id, name, off, steps: [{id, n, name, kind, state, runs, failures, repairs, answer, item?, extra: {}}]}], current, ended}`.
  A step's `state` is `done`, `repaired` (met after a repair), `again` (run again after a re-entry, a second result or a failed
  attempt), `failed`, `skipped`, `off` (the settings' `skip`, the run configuration's `switched_off`, or `outside`/`planned`),
  `running`, `ahead` (not yet reached by a live or stopped run), `notrun` (an ended run never reached it); a checkpoint's `answer`
  is `yes`, `no` or `auto`. An `each`'s steps appear once per item (`<id>-<item>`). A resumed run's replay is not a second run.
- **timeline**: a row per step result (`minutes` since the result before it, `paused` between launches, `repairs`), and `wall`,
  `waiting` (for humans), `paused`, `repairs`, `task_force`, `checkpoints_asked`.
- **profile**: per step the substeps (`parts`: do, check, critique, repair, hooks, task force, human) with their time on the run's
  axis, verdict and cost; `segments` for the time axis; `used` (agent and model per session, from `report.json`); `files.outputs`
  (what the step's commit changed); and `tokens`, `cost`, `task_force`, `map`, `pending` (the running step's substeps).

`steps[].extra` is left empty: a caller merges its own facts there (a waived gate's "unmeasured"), or the process's
`ui.overlay` does (below). `rlt serve` answers `GET /api/process?report=map|timeline|profile` (404 when the project ran no
process: `rlt process` notes its file in `rlt/private/process.json`), the web UI has a page for it (`/process`), and
`rlt status` prints the running step and phase.

### The web UI of a process (A-39)

`rlt serve --projects <folder> --process <file>` serves the projects of a folder (each request names one with `?p=<name>`;
the projects listed are those that ran a process) and starts new runs with `<file>`; a project's own run keeps the file it
last ran. The page (`/process`) lists the projects, makes a new one from the process's form, and for one project shows
the controls (Start fresh, Resume, Stop), the checkpoints waiting for a human with their document, the reviewers' open
points and the decisions so far, while it runs what it does now (its step, agent, current activity, how long it has
been quiet), the session's activity and the critics' last verdicts, and the map and profile. Each action is what a human does in the terminal:

| Endpoint | Does |
|---|---|
| `POST /api/run {action}` | `start`: `rlt process <file> <project>` in the background (output in `rlt/private/run.log`); `resume`: the same with `--resume`; `stop`: SIGTERM to the run (its session is interrupted, `resume` continues). 409 when a run is alive (start, resume) or none is (stop). |
| `GET /api/checkpoint` | while a run is alive: each open ask whose question names a checkpoint's decisions file, with `step`, `doc`, `document`, `decisions_file`, `decisions`, `open_points_file`, `open_points` |
| `POST /api/answer {id, value, decisions?}` | the decisions, one list item per line, are appended to the checkpoint's decisions file, then the ask is answered |
| `GET /api/reviews` | the critics' last eight verdicts (`<step>-review` in `rlt/private/verdicts.jsonl`), with `step`, `verifier`, `met`, `rationale`, `ts` |
| `GET /api/projects`, `GET /api/ui`, `POST /api/projects {name, <fields>}` | the projects; the form; a new project made by the form's command, then its run started |

The process file's `ui:` section declares what the page shows beyond rlt's own report:

```yaml
ui:
  new_project:
    command: scripts/new-project.sh   # run with the new project's path; each field as RLT_FIELD_<NAME>; it creates the folder
    fields:
    - { name: task, label: The task, hint: what the run should build, required: true, kind: textarea }
    - { name: size, label: Size, kind: select, options: [small, large] }   # kind: text (default), textarea, select
  overlay: scripts/overlay.sh         # run in the project with <project> <kind>, the report's JSON on stdin
```

The overlay prints `{"steps": {<step id>: {<key>: <value>}}, "run": {<key>: <value>}}`; rlt merges each step's facts into its
`extra` (the map's and the profile's) and the run's into the report's `extra`, and the page shows them as key and value rows.
A failing overlay leaves the report as rlt made it, with `overlay_error`. Not part of it: what the facts mean (a proposals
estimate, waived gates, the roles a step ran with) is the process's own.

`--resume` continues the run that was started last, with its arguments (`rlt/private/started.json`): also one a re-entry
started at a step with its note.

### Scenarios (A-38)

`rlt process <file> --scenario <scenario.yaml>… --projects <dir> [--keep]` plays the process with scripted agents, no tokens:
each scenario names a project of `<dir>` (copied to a scratch folder, git history included), the `args` to run
(`--from`, `--until`, …), the `agents` (rules: `when` is a regex on the prompt; `write` files, a `run` shell command, an
`answer` file for a verifier or critic) and the `answers` to asks (`ask` regex, `say`, `decisions` appended to the file the
question names). Agents are the `claude`/`codex` wrappers `rlt scripted-agent` serves; a rule's `run` sees
`$RLT_SCENARIO_PROMPT` (the prompt's file), and `RLT_SCENARIO` / `RLT_SCENARIO_LOG` name the scenario and its call log.
An ask no answer matches is answered no. `expect` checks the run: `record` (regexes in order over the history lines,
`{label:<id>}` is a step's label), `never`, `sessions` (`<agent> <role>` lines), `files` (path → regex), `exit`. Prints
`PASS <name>` or `FAIL <name>: <what>` with the run's record and calls; exit 1 if any failed. Unix only.

### Exit codes and the record

`0` done, `3` halted (an escalation, a human's no, a failed `--from`), `6` a step's task-force limit is spent, `1` an
error. The history is JSON lines of `{step, id, kind, met, rationale, at, commit}`; kinds include `done`, `unchanged`,
`tool`, `skipped`, `approval`, `auto-approval`, `repair`, `tier`, `n/a`, `task-force`, `reentry`, `hook`.

## When it isn't the right tool

A process file composes rlt's blocks; it can't branch on what the run learned, bound a loop by a computed value or keep
state in a variable. When the flow needs that, write the driver in Rust on [`relentless::workflow`](RUST_API.md#workflow-building-blocks-aggworkflow)
— the process file and the Rust blocks share one implementation, so nothing is lost by starting in YAML.
