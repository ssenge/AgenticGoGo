<h1 align="center">
  <img src="assets/logo.png" width="170" alt="Relentless: a macaw inside a loop arrow"><br>
  Relentless
</h1>

<p align="center"><strong>The Agentic Workflow Engine</strong></p>

Relentless runs agentic work as a process: agents do the work, verifiers they can't fool decide when it is done, and
humans decide only where they must.

## Why

- Coding agents are stochastic, and they grade their own homework: "done" is whatever the last message said.
- Long work stalls, runs out of context, or stops one step short, and you end up typing "continue" into a terminal.
- Relentless puts a deterministic process around the agents: steps, **verifiers** that decide "done" (the agent never
  runs them and can't edit them), checkpoints where a human approves, and resume from anywhere after a crash or a stop.
- The process is a file in git, so it is reviewed, versioned and repeatable. The agents stay agents; the workflow
  around them becomes code.

## A first workflow

`examples/hello/` is the smallest useful process: an agent fixes a wrong program, a script verifier and a reviewing
agent check it, and a human approves.

```yaml
# examples/hello/process.yaml
version: 1
roles: { file: tools.yaml }          # in the project: which agent works, which one reviews
phases:
- id: build
  name: Build
  steps:
  - id: fix
    name: Fix add
    kind: agent
    review: true                     # a second agent grades the result against verifiers/fix-review.md
    checks: [verifiers/tests.sh]     # a script verifier: its last line is {"met": …, "rationale": …}
    settings: { inputs: [task.md, cp-fix-decisions.md], outputs: [add.py] }
  - { id: cp-fix, name: Fix approved, kind: checkpoint, doc: add.py,
      question: "{{ cp.cp-fix }}: approve {{ step.fix }}?", owner: fix }
```

Run it on a project (a git repository holding `task.md`, `add.py` and `tools.yaml`; copy `examples/hello/projects/demo`):

```bash
rlt process examples/hello/process.yaml path/to/project
```

The agent works in a fresh session on its own branch. Then Relentless runs `verifiers/tests.sh` itself. A verifier that
isn't met goes back to the agent with its reason, and the work is not kept until the checks pass. A second agent
reviews. The run then stops at the checkpoint and waits for your answer (`rlt answer`, or the web UI).

**Try it without tokens.** A scenario plays the same process with scripted agents:

<!-- check -->
```bash
rlt process examples/hello/process.yaml --scenario examples/hello/scenario.yaml --projects examples/hello/projects
```

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ssenge/relentless/main/scripts/install.sh | sh
```

Or from source: `cargo install --git https://github.com/ssenge/relentless relentless`. You need `git` and one coding agent
CLI that can run headless: [Claude Code](https://claude.com/claude-code),
[OpenAI Codex](https://developers.openai.com/codex/cli) or [GitHub Copilot](https://github.com/github/copilot-cli).

```bash
rlt doctor     # the agent is on PATH, and can do what your config asks
```

→ [All install options](docs/INSTALL.md) · [choosing an agent](docs/GUIDE.md#choosing-an-agent)

## Verifiers

A verifier decides whether one goal is met. It is the Definition of Done, made executable.

- **Script verifiers.** Any script that prints a verdict as the last line of its output:
  `{"met": true, "rationale": "17 of 17 tests pass"}`. A value and a target make it a number the ratchet can hold.
- **Rubric verifiers.** A `.md` file is a rubric; an agent grades the work against it and answers with the same verdict.
  Use one where only judgement can decide, and keep the rubric narrow.
- **Declared verifiers.** A command met by its exit code, a metric of the code (complexity, coverage, duplication) with
  a bound, or a template a document must follow.
- **Conditions.** `done_if: "tests AND docs"` says what *done* means; `abort_if` says when to give up. A ratchet turns
  "never worse than the last kept result" into a gate.

Verifiers are committed with the project. If a worker changes one, the change is rolled back. They run in the run's
isolation, their verdicts are cached on the content they saw, and the agent never decides that it is done.

→ [Writing verifiers](docs/GUIDE.md#building-verifiers) · [full config reference](docs/CONFIG.md)

## Process files

A process file (`rlt process`) is a list of phases; each phase is a list of steps of a closed set of kinds:

| Kind | What it does |
|---|---|
| `script` | runs a command; its verdict is the step's |
| `agent` | an agent's task, with `checks` (verifiers), a `review` (a critic agent) and repairs |
| `checkpoint` | a human approves a document; their decisions go back to the step that wrote it |
| `each` | the same steps once per item of a list |
| `exam` | an examinee, a grader and a repairer, in rounds |

Around the steps: **ladders** (a cheap agent first, a stronger one when the cheap one fails), a **task force** that
finds the cause of a failed step and repairs it in place, re-enters an earlier step or asks a human, **re-entry** with
a decision, hooks, a verdict cache, and **scenarios** that play the whole process with scripted agents (no tokens) so
control flow can be tested.

→ [Process files](docs/PROCESS.md) · [`examples/process/`](examples/process/)

## Running it

- `rlt dashboard`: the live TUI. `rlt serve`: a web UI with the process map, open asks, the reviews and the report.
- `rlt process … --report timeline`: what ran, when, and what each step cost.
- **Resume:** `--resume` continues an interrupted run from its call ledger without re-spending a token. A stop
  (`rlt stop`, Ctrl-C) keeps the work.
- **Rate limits:** a limited agent makes the run wait for the reset (`rlt send wake` ends the wait), and it never
  burns attempts on the wait.
- `rlt status --json` gives programs the run's state; `rlt answer` answers an open ask from any shell.

## Agents and where they run

| Agent | `agent:` | Notes |
|---|---|---|
| Claude Code | `claude` | the default; usage and cost reported |
| OpenAI Codex | `codex` | |
| GitHub Copilot CLI | `copilot` | |

A worker's confinement is one of three **isolation** tiers, set per step:

| Tier | What it is |
|---|---|
| `none` | the agent's own permission model |
| `sandbox` | an OS jail (Seatbelt on macOS, bubblewrap on Linux): writes only to the project and the temp folder |
| `container` | Docker or Podman, with an egress allow-list |

→ [Adding an agent](docs/BACKENDS.md) · [isolation](docs/CONFIG.md)

## Drive it from Rust

When the flow has to branch (skip a step on a verdict, or not run a slow verifier until three cheap ones pass), write
it as a Rust program against the same engine:

```rust
let rlt = Relentless::open(".")?.limits(limits).on_regression(OnRegression::Rollback);
for _ in 1..=20 {
    rlt.check_limits()?;
    rlt.step(&implement)?;                    // stages on a session branch; nothing merged yet
    if !(rlt.verifier(&tests).met() && rlt.verifier(&load).met()) {
        continue;                             // `&&` is the cost gate: `load` never runs on a red build
    }
    rlt.gate()?;                              // land the whole span, or discard it
}
```

→ [The Rust driver API](docs/RUST_API.md) · [`examples/workflow.rs`](examples/workflow.rs)

## Learn more

| | |
|---|---|
| [docs/README.md](docs/README.md) | the map of the docs |
| [docs/GUIDE.md](docs/GUIDE.md) | the full walkthrough |
| [docs/PROCESS.md](docs/PROCESS.md) | process files: steps, checks, ladders, the task force, scenarios |
| [docs/CONFIG.md](docs/CONFIG.md) | every `rlt.yaml` key, and what a verifier may write |
| [docs/RUST_API.md](docs/RUST_API.md) | the driver API and resume |
| [docs/BACKENDS.md](docs/BACKENDS.md) | adding an agent backend |
| [docs/CONCEPTS.md](docs/CONCEPTS.md) | the ideas behind it |
| [AGENTS.md](AGENTS.md) | the reference for an agent working on this repository |

## Where it comes from

The [Ralph loop](https://ghuntley.com/ralph/) showed that a fresh agent session in a deterministic outer loop beats one
long conversation. Relentless adds the missing half, that the agent never decides it is done, and builds on it:
**Loop Engineering** (the loop is code, not a conversation), **Graph Engineering** (a loop laps in a fixed order; a graph
chooses) and **Agentic Workflows as Code** (the workflow is a reviewable artifact). → [docs/CONCEPTS.md](docs/CONCEPTS.md)

## Licence

MIT
