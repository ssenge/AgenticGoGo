<h1 align="center">
  <img src="assets/logo.png" width="170" alt="AgenticGoGo — a pole-dancing robot that keeps your agent going going"><br>
  AgenticGoGo
</h1>

<p align="center">
  <em>Define your agent workflow as code — YAML or Rust — and let a deterministic outer loop<br>with incorruptible judges drive it to done.</em><br>
  <em><strong>Loop Engineering</strong> · <strong>Graph Engineering</strong> · <strong>Agents as Code</strong></em>
</p>

<p align="center"><em>Stop typing “go go”.</em></p>

---

Are you constantly typing **“go go”**, **“continue”**, **“keep going”** to nudge your coding agent
through a long plan? Do even spec-driven approaches stall mid-flight, run out of context, or
quietly stop one step short — leaving you to babysit a terminal?

**Then AgenticGoGo is for you.**

AgenticGoGo (`agg`) is a framework for **Loop Engineering** (e.g. Ralph Loops), **Graph Engineering** and **Agents as
Code**. You define the workflow as a versioned artifact — in **YAML** for a sequence that laps, or in
**Rust** when the flow has to branch — and `agg` runs it as a deterministic outer
**[Ralph loop](https://ghuntley.com/ralph/)** around a stochastic inner coding agent (currently
supported: [Claude Code](https://claude.com/claude-code), [OpenAI Codex](https://developers.openai.com/codex/cli), [GitHub Copilot](https://github.com/github/copilot-cli)) — relaunching a **fresh** session each cycle, verifying its work against gates
*it can't fake* (the **judges**), and repeating until your goals are actually met. The loop is plain code: it never hallucinates a decision. The agent does the work, inside one step, and never decides when it's done. *(A similar LLM-based approach — generate → verify → keep, as in evolutionary code search — was
proposed years ago, outside the Ralph-loop community, by DeepMind's
[AlphaCode](https://arxiv.org/abs/2203.07814) and its open-source variant
[CodeEvolve](https://arxiv.org/abs/2510.14150).)*

A **judge** is a small, incorruptible check that decides whether one goal is met — usually a script
inspecting the artifact (tests, a compiler, a proof checker), or an LLM grading against a rubric. You
compose several with a boolean grammar (`and` / `or` / `not`, e.g. `outputs_two and tests_pass`) to
say exactly what *done* means.

**A loop alone is not enough, and this is the part most agent tooling leaves out.** Two more ideas
carry equal weight in `agg`, and every design decision here follows from one of the three:

- **[Agents as Code](#2--agents-as-code--your-workflow-is-a-reviewable-artifact)** — your workflow is
  **committed source**, not a prompt in someone's terminal history. `agg/agg.yaml` + `agg/judges/*`
  are diffed and code-reviewed like anything else; in Rust it is a **compiled program you can
  unit-test**. It is also what makes the judges incorruptible: they live in git, so a run that
  tampers with one is rolled back to the committed version.
- **[Graph Engineering](#3--graph-engineering--knowledge-that-survives-a-fresh-session)** — because
  every session starts **fresh**, what the run learned must live somewhere durable *and navigable*.
  `agg/state/wiki/` is a **knowledge graph** (one concept per file, typed, cross-linked), not an
  append-only log — so the next session enters at the right node instead of re-reading everything.

The loop gives you determinism · Agents-as-Code makes it reviewable · the graph gives it a memory.
[Read the three in full ↓](#three-ideas-it-is-built-on)

<p align="center">
  <img src="assets/loop.png" alt="The four stages of the agg loop — INJECT, RUN, VERIFY, GATE — arranged in a circle" width="620">
</p>

| Stage | What it does | Who runs it |
|---|---|---|
| **`INJECT`** | Builds the agent's prompt: your standing instruction, what past sessions learned, any steering you queued. | code |
| **`RUN`** | Launches one **fresh** agent session (`claude -p` · `codex exec` · `copilot -p`). It edits files. It never decides whether it succeeded. | **the agent** |
| **`VERIFY`** | `agg` runs your **judges** itself. The agent is never asked to grade its own homework. | code |
| **`GATE`** | Keeps or rolls back the work, checks `done_if`, carries state forward — or stops. | code |

Three of the four stages are deterministic code; only the `RUN` stage is a (stochastic) coding agent.
The loop continues until your Definition of Done is met — potentially for hours, days, weeks (watch
your token consumption 😉). Because the agent never runs `VERIFY`, it can't fake the gate that decides
it's done.

**Your judges *are* that gate — your Definition of Done (DoD), made executable.** You compose them
into one expression, `done_if`, that says exactly what *done* means. Good judges are built on
quantifiable goals: *"`solve(Y)` returns `X`"*, *"18 of 28 benchmarks pass"*, *"`f(x)` runs in under
200 ms"*, *"the report scores ≥ 85% against this rubric"* — or any boolean combination of such judges
(`done_if: "solves AND fast_enough"`). A vague DoD like *"make the code nicer"* gives an LLM-based
assessment far too much slack, and is easily gamed.

The overall architecture is captured in the following diagram:

<p align="center">
  <img src="assets/arch.png" alt="AgenticGoGo architecture: the agg outer loop drives one fresh agent worker (claude -p / codex exec / copilot -p) and writes plain state files under agg/state/ and agg/private/, which the TUI, the web UI, and an agent supervisor session (reachable from your phone) all read" width="760">
</p>

The whole system is **the loop plus plain files**: `agg` drives one fresh worker, which writes its own
state to `agg/state/` while `agg` writes the run's ledger and scoreboard to `agg/private/`; the TUI, the
web UI, and an `/agg:supervise` agent session (reachable from your phone, via Claude
Code's mobile app) all just *read* those files (and the supervisor can *steer* via the bus). More on that in
the [guide](docs/GUIDE.md).

## Three ideas it is built on

### 1 · Loop Engineering — the loop is code, not a conversation

[Ralph](https://ghuntley.com/ralph/) is the insight that a *fresh* agent session in a deterministic
outer loop beats one long conversation: no context rot, no drift, no "where were we". `agg` is that
loop with the missing half added — **the agent never decides it is done.** Judges do, and the agent
cannot run them.

### 2 · Agents as Code — your workflow is a reviewable artifact

The loop, the steps, the agents, the Definition of Done and the judges are **files in your repo**:
`agg/agg.yaml` and `agg/judges/*`, committed, diffed and code-reviewed like anything else. A prompt
in someone's terminal history is not reproducible; this is. It is also *why the moat holds* — the
judges are committed, so a run that tampers with one gets rolled back to the version in git. When
YAML stops being enough the same idea goes further: the workflow becomes a **compiled Rust program**
you can unit-test.

### 3 · Graph Engineering — knowledge that survives a fresh session

Every session starts clean, so what the run *learned* has to live somewhere durable and
*navigable*. `agg/state/wiki/` is an **OKF (Open Knowledge Format) knowledge graph**:
one concept per markdown file, typed frontmatter, cross-linked with ordinary
`[label](page.md)` links. Not an append-only log — a graph the next session can *enter at the right
node*. Plans, decisions and dead-ends go there and survive rollbacks. Open `agg/` as an
[Obsidian](https://obsidian.md) vault to see it.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/scripts/install.sh | sh
```

Then the `/agg:*` skills — all three agents share one marketplace:

```
/plugin marketplace add ssenge/AgenticGoGo     # Claude Code (Codex + Copilot: see the guide)
/plugin install agg@agenticgogo
```

You also need a coding agent `agg` can drive **headlessly** — [Claude
Code](https://claude.com/claude-code), [OpenAI
Codex](https://developers.openai.com/codex/cli) or [GitHub
Copilot](https://github.com/github/copilot-cli). Then:

```bash
agg doctor       # verifies the agent is on PATH *and* can do what your config asks
```

→ [Full install options](docs/INSTALL.md) · [choosing an agent](docs/GUIDE.md#choosing-an-agent)

## A loop in YAML

Two files. `agg/agg.yaml` says what to run and when it is done; `agg/judges/*` decide *done*.

```yaml
# agg/agg.yaml
project: calc
defaults: { agent: "claude" }
steps:
  worker:
    prompt: "Fix the failing tests in calc.py. Do not edit the tests."
sequence:
  steps: [worker]                  # a list that laps, forever
  done_if: "tests_pass"            # …until THIS is true
  limits: { sessions: 40 }
```

```bash
#!/usr/bin/env bash
# agg/judges/tests_pass.sh — agg runs this; the agent never does
python3 -m pytest -q >/dev/null 2>&1
printf '{"met":%s,"target":1,"rationale":"pytest"}\n' "$([ $? -eq 0 ] && echo true || echo false)"
```

```bash
agg run                            # …and watch it: agg dashboard
```

That is the whole model: **a sequence that laps until the judges say done.** Add bounded repetition
(`until:` + `max:`), per-step agents and models, sandboxing, notifications — all still YAML.

→ [Writing judges](docs/GUIDE.md#building-judges) · [steps and
sequences](docs/GUIDE.md#steps-and-sequences) · [full config reference](docs/CONFIG.md)

## A driver in Rust

YAML is a list that laps. When the flow needs to **branch** — skip a step on a verdict, or refuse to
run a 40-minute judge unless three cheap ones passed — it becomes an ordinary Rust program against
the *same engine*:

```rust
let agg = Agg::open(".")?.limits(limits).on_regression(OnRegression::Rollback);

for c in 1..=20 {
    agg.check_limits()?;
    agg.step(&implement)?;                     // stages on a session branch; nothing merged yet
    if !(agg.judge(&tests).met() && agg.judge(&load).met()) {
        continue;                              // `&&` IS the cost gate: `load` never runs when red
    }
    agg.gate()?;                               // land the whole span, or discard it per policy
}
```

Judges are **lazy and memoized per step**, which is what makes `&&` a real cost gate. `step()`
stages, `gate()` lands. A crashed run resumes from a call ledger without re-spending a token.

**Heavier and Rust-only — use YAML unless you need flow it cannot express.**

→ [The Rust driver API](docs/RUST_API.md) · working drivers:
[`examples/workflow.rs`](examples/workflow.rs),
[`examples/selfimprove.rs`](examples/selfimprove.rs)

## Where to go next

| | |
|---|---|
| [**docs/GUIDE.md**](docs/GUIDE.md) | the full walkthrough — install, agents, judges, sequences, state and memory, interfaces, CLI |
| [docs/CONFIG.md](docs/CONFIG.md) | every `agg.yaml` key, and what a judge may write |
| [docs/RUST_API.md](docs/RUST_API.md) | the driver API, resume, and what a driver author must design around |
| [docs/INSTALL.md](docs/INSTALL.md) | prebuilt binaries, from source, version pinning |
| [examples/](examples/) | a full YAML workflow, two Rust drivers, and a research loop |
| [AGENTS.md](AGENTS.md) | the condensed reference for an agent working **on** this repo |

## License

MIT
