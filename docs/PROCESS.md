# Process files — `agg process`

A **process** is phases of steps. Each step is one of a closed set of kinds: a script, an agent's work judged by checks
and a critic, a human's approval, a loop over a list, an exam. `agg process <file> <project>` runs it from YAML. It is the
YAML twin of the Rust driver blocks in [`agg::workflow`](RUST_API.md#workflow-building-blocks-aggworkflow): every entry
becomes a call of the block that already exists. **The YAML has no logic of its own** — no expressions, no `if:`, no
variables beyond the ones listed here. A condition is a script's verdict; real logic is a script or a Rust driver.

Reach for it when the flow is a *process* (steps in order, each judged, humans at checkpoints) rather than one loop of
sessions on a goal (that is `agg.yaml`, [CONFIG.md](CONFIG.md)). The two are separate: `agg.yaml` is unchanged.

```
agg process <file> <project> [--resume] [--from <step>] [--note <text> | --note-file <f> [--decided]]
                             [--reset] [--until <step>] [--reentries <n>]
agg process <file> --check     # validate the file; a verdict JSON last, exit 1 on any problem
agg process <file> --list      # the numbered steps as JSON
```

`examples/process/` is a complete, commented example (it is also what the test suite runs).

## Identity, numbers and names

A step is **its id** (`lower-case-with-dashes`) everywhere: the history, `refs/agg/before/<key>`, the task force's count,
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
dirs: { prompts: prompts, reviews: judges }
settings:
  project: settings.yaml              # the project's overlay file (in the project)
  extra_keys: [other]                 # keys of that file agg tolerates
  defaults: { repairs: 1 }            # per-step settings, see below
  skip: [extras]                      # ids of phases or steps kept in the order, not run
  quality: { definition: d, version: "1", min: 90, guide: prompts/quality.md, reports: judges/reports.py, scope: "{{ app }}" }
  models: { small: some-model }       # sizes a ladder's tier may name
agg: { memory: false, user_settings: false, on_regression: annotate, judge_timeout: 3600 }
run:
  history: reports/steps.jsonl        # the step history (default agg/private/steps.jsonl)
  own: [task.md]                      # files --reset never takes back
  start: [scripts/start.sh]           # before agg opens the project; the last stdout line is logged
  config: run.yaml                    # `switched_off: { <id>: <why> }`, written by a step
  bases_reset_when_changed: increment.yaml
  system_prompt: prompts/system.md
  brief:
    sections: [{ file: prompts/house-rules.md }]   # in every agent's brief; {{ this }}, {{ this.id }} …
    inputs: { always: [notes.md], max_file: 16000, max_all: 48000 }
  critic_scope: prompts/critic-scope.md            # {{ outputs }} is the step's outputs
  open_points: checkpoints/open-points.md
  leftovers: true                     # end processes a step left running
  judge_cache: { ignore: [notes.md] }
  hooks: { process: hooks.yaml, project: hooks.yaml }
  auto_note: scripts/waiting.sh       # stdout appended to an auto-approval's record
deny: { inputs: [task.md] }           # named write-deny lists
task_force:
  prompt: prompts/task-force.md
  max: 3                              # re-entries and repairs per failed step
  deny: [inputs]
  sections: [{ text: "…" }]
  trust: [{ failed_in: build, reenter: plan, except: [deploy] }]   # proposals that go ahead without a human
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
| `meta` | free-form, ignored by agg, handed through by `--list` (descriptions for docs and slides) |

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
script gets `AGG_STEP` (the key), `AGG_STEP_LABEL`, `AGG_PROCESS_HOME` and, in an `each`, `AGG_ITEM`. A check prints a
verdict JSON as its last line. A check is a command, or `{ run, name, when: last }` (`last`: only the last item's instance
of an `each` runs it). Check judges are named after the step and the command unless `name` is given; every script check
goes through the judge cache.

**Sections** (`sections:`) are `{ text }`, `{ file }` (read and rendered) or `{ run }` (a command's output, run when the
brief is built). A step's own sections are appended to its task (the critic sees them); the run's `brief.sections`, the
small input files and the project's note are the worker's only.

**`agent`** — an agent step in order: a refresh whose `settings.inputs` didn't change is `unchanged` without a session;
`settings.first` does what it can; the worker's brief gets what is new first, then the task, the sections, the critic's
rubric and the checks; the checks, then the critic. A critic's point left after its round is kept for the next
checkpoint (`run.open_points`). `quality: true` adds agg's score judge on the files changed since the step's base. `on_fail`
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
`outputs`, `tools`, `first` (a command), `ladder`, `note`. The project's file also has `skip` (replacing the process's),
`quality`, `models`, `managed` (tools a ladder's decider may pick besides `ladder.tools`) and `auto_approve`. Unknown keys
are errors; every id and name it uses must exist in the process.

A **ladder** is the step's tiers, first to last: `{ mode: once | dcr | dccr | converge, model, effort, agent, repairs, max }`,
`{ tool: <name> }` (runs `ladder.tool_run`; a verdict with `value: 0` means no tool applied: recorded `n/a`) or `agent`
(a cheap agent picks the next tier). The first tier that meets the step's checks and critic ends it; the last fails into
the task force. A re-entry with a note starts one tier higher.

### The task force, hooks, the cache

A failed step goes to the task force (`task_force`): an agent that finds the cause, repairs it in place or proposes a
re-entry (`reenter`, an id) or escalates. The limit holds per failed step and survives restarts; a spent limit exits **6**.
`trust` rules let a proposal go ahead without a human. A hooks file has `step:`, `steps: { <id>: … }` and
`phases: { <id>: … }`, each `before`/`after`: scripts that run around every step or phase and print a verdict; the project's
file adds to the process's.

`agg cached [--ignore <path>]… -- <command>…` (the process wraps script checks in it) answers a met verdict from an earlier
run of the same check on exactly the same project content; unmet verdicts are never cached.

### Templates

`{{ name }}` in the file's strings and in the files it reads is filled in from: `vars`/`vars_file`, `home`, `project`,
`step.<id>`, `step.<id>.number`, `step.<id>.name`, `cp.<id>`; in a step's strings also `this`, `this.id`, `this.key`,
`this.name`, `this.number`, and in an `each` `item`, `item.yaml`, `item.<field>`, `before`, `base`; in `vars` values
`env.NAME`. In the file's own strings an unknown name is an error; a prompt's own template language (`{{ a.b }}`) is
left alone unless the name is `step.…`/`cp.…`, which `--check` verifies.

### Exit codes and the record

`0` done, `3` halted (an escalation, a human's no, a failed `--from`), `6` a step's task-force limit is spent, `1` an
error. The history is JSON lines of `{step, id, kind, met, rationale, at, commit}`; kinds include `done`, `unchanged`,
`tool`, `skipped`, `approval`, `auto-approval`, `repair`, `tier`, `n/a`, `task-force`, `reentry`, `hook`.

## When it isn't the right tool

A process file composes agg's blocks; it can't branch on what the run learned, bound a loop by a computed value or keep
state in a variable. When the flow needs that, write the driver in Rust on [`agg::workflow`](RUST_API.md#workflow-building-blocks-aggworkflow)
— the process file and the Rust blocks share one implementation, so nothing is lost by starting in YAML.
