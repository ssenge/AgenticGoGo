# The agg guide

Everything past the [README](../README.md)'s two examples: installing, choosing an agent, writing
judges, sequences, state and memory, the interfaces and the full CLI.

> Config reference → [CONFIG.md](CONFIG.md) · Rust driver API → [RUST_API.md](RUST_API.md)

---

## Quick start

Say you have a project with a broken `calc.py` and you want it fixed:

```python
# calc.py — BROKEN on purpose
def add(a, b):
    return a * b            # bug: should be a + b

if __name__ == "__main__":
    print(add(1, 1))        # prints 1; should print 2
```

*(A deliberately trivial example — a one-line fix where `agg` buys you nothing over just doing it
yourself. It's here to show the mechanics; the real payoff is long, multi-hour work you'd otherwise
have to babysit.)*

**0 — Install.** The binary:

```bash
curl -fsSL https://raw.githubusercontent.com/ssenge/AgenticGoGo/main/scripts/install.sh | sh
```

Then the `/agg:*` skills. **All three agents have a plugin marketplace, and all three take the same
one** — add the marketplace, install the plugin:

```
# Claude Code — inside a session
/plugin marketplace add ssenge/AgenticGoGo
/plugin install agg@agenticgogo
```
```bash
# OpenAI Codex
codex plugin marketplace add https://github.com/ssenge/AgenticGoGo
codex plugin add agg@agenticgogo
```
```bash
# GitHub Copilot
copilot plugin marketplace add ssenge/AgenticGoGo
copilot plugin install agg@agenticgogo
```

Or install them straight into **this project**, with the binary you just installed — no marketplace,
same three skills, every agent:

```bash
agg skills install --agent codex     # claude | codex | copilot · --user for account-wide
                                     # (agg needs to know WHICH agent — it installs into a
                                     #  different directory for each, and this runs before
                                     #  agg.yaml exists. Inside an agent's own shell it detects it.)
```

Full options (prebuilt binaries, from source, version pinning) →
**[docs/INSTALL.md](INSTALL.md)**.

**1 — Prerequisite: a coding agent.** `agg` drives one headlessly. Install and authenticate whichever
you want, and check it works:

| agent | `agg.yaml` | install + authenticate | check it works headlessly |
|---|---|---|---|
| **[Claude Code](https://claude.com/claude-code)** *(default)* | `agent: claude` | `npm i -g @anthropic-ai/claude-code` · `claude auth login` | `claude -p "hello"` |
| **[OpenAI Codex](https://developers.openai.com/codex/cli)** | `agent: codex` | `npm i -g @openai/codex` · `codex login` | `codex exec "hello"` |
| **[GitHub Copilot CLI](https://github.com/github/copilot-cli)** | `agent: copilot` | `npm i -g @github/copilot` · `copilot login` | `copilot -p "hello"` |

The **check** column is the one that matters: `agg` only ever drives the agent *headlessly*, so a
version number or a login status proves nothing. If that one-shot prompt answers, `agg` can drive it.

`agg doctor` verifies the agent is on your PATH **and** that it can do what your config asks —
run it before your first loop. See [Choosing an agent](#choosing-an-agent) for what each one
can't do.

**2 — Let the agent set up the loop.** Invoke the skill: **`/agg:new`** (Claude Code), **`/agg-new`**
(Copilot), **`$agg-new`** (Codex — it uses `$`, not `/`). On any of them you can equally just *ask*
("set up AgenticGoGo for this project") and the agent picks the skill up by its description. Either
way it turns *your* DoD into config. Point it at a spec you already have (a PRD, ROADMAP, README,
`.planning/`), or just say what you want — e.g.

> `/agg:new` — done = `python3 calc.py` prints `2` **and** `pytest -q` passes

It reads that plus your code, shows the `agg/agg.yaml` it proposes, and lets you edit before writing.
Each judge is a small file in `agg/judges/` named for the check it makes — a judge that passes IS a
met goal. The config the skill writes might look like:

```yaml
# agg/agg.yaml — the whole config: defaults / judge / steps / sequence
project: "calc"

defaults:
  agent: "claude"                  # claude (default) · codex · copilot — see "Choosing an agent"
  model: "claude-opus-4-8[1m]"     # claude only. On codex: OMIT this line. On copilot: `auto`.
  state: "state/STATE.md"          # the worker's forward "what to do next" advice

judge:                             # THE RULER — runs LLM judges + the summarizer
  agent: "claude"
  model: "claude-haiku-4-5-20251001"   # a cheap model grades; the worker stays on Opus
  timeout: 300

steps:
  worker: {}                       # one plain worker step

sequence:
  steps:
    - "worker"                     # run `worker`, forever, until done_if fires
  done_if: "outputs_two AND tests_pass"        # your Definition of Done — a boolean over judge names
  abort_if: "over_iterations OR wall_hours >= 1"   # a ceiling, not part of the DoD
```

Each fresh session's entire `-p` is now a tiny fixed pointer — *"read `agg/private/INSTRUCTIONS.md`
and follow it"* — and `agg` **composes that file anew at every `INJECT`** from the step's role + its
`prompt:`, a recent tail of memory, any queued steering, and pointers to two files: `agg/AGG.md`
(the stable scope/goals — "fix `calc.py` so `python3 calc.py` prints 2 and the tests pass") and
`agg/state/STATE.md`, the forward "what to do next" advice the agent rewrites each session to track
progress (see [State and memory](#state-and-memory)).
Note the `done_if: "outputs_two AND tests_pass"` line: it composes two judges with `AND`, and each
name resolves to a file in `agg/judges/`. The next step writes them.

**3 — Have the agent write the judges.** A judge is a **file named for the judge** that prints a
verdict as JSON — `outputs_two` → `agg/judges/outputs_two.sh` (see [Building judges](#building-judges)).
You can write one by hand, but asking your agent is the easy way:

> **Prompt:** Write the judge `agg/judges/outputs_two.sh`: run `python3 calc.py`, print `{"met":true}` if its
> output is exactly `2`, else `{"met":false,"rationale":"calc.py did not print 2"}`. Nothing else on
> stdout. And `agg/judges/tests_pass.sh`: `pytest -q` must exit 0.

The result might look like:

```bash
#!/usr/bin/env bash
# agg/judges/outputs_two.sh — VERIFY. agg runs this; the agent never does.
[ "$(python3 calc.py 2>/dev/null)" = "2" ] \
  && echo '{"met":true}' \
  || echo '{"met":false,"rationale":"calc.py did not print 2"}'
```

```bash
#!/usr/bin/env bash
# agg/judges/tests_pass.sh — the whole suite is green
pytest -q >/dev/null 2>&1 && echo '{"met":true}' || echo '{"met":false}'
```

The `done_if: "outputs_two AND tests_pass"` line requires **both**. Why chain them? Each catches
what the other misses: `outputs_two` alone is gameable — the agent could just hardcode `print(2)` —
but `tests_pass`, which checks `add(2, 3) == 5`, would still be red. That `AND` is judge chaining:
any boolean of judge names (`AND` / `OR` / `NOT`, with parentheses) is a valid `done_if`. (Need a
numeric threshold? Use the accessor: `done_if: "tests_pass AND coverage.value >= 80"`.)

**4 — Run it, and watch.**

```bash
agg plan                # dry run: one VERIFY pass, prints the scoreboard. No agent launched.
agg run --detach        # drive the loop until done_if is met; logs to agg/private/run.log
agg dashboard           # live TUI  (or: agg serve + the web UI — see Interfaces)
```

`VERIFY` rejects the broken `calc.py` → the agent fixes `a * b` → `a + b` → `VERIFY` re-runs both
judges → `GATE` sees `done_if` met → the loop stops. This toy finishes in a **single iteration**,
and there's no knob to force more — the loop stops the moment `done_if` is true. Real projects run
as many iterations as it takes to satisfy every clause, sometimes hundreds, each a fresh session.

**5 — Optionally, supervise from a second agent session.** Open a second session **in the same project
folder** and invoke the supervisor skill — `/agg:supervise` (Claude), `/agg-supervise` (Copilot),
`$agg-supervise` (Codex). It's not required — a plain session could read the state and run `agg send`
too — but the skill hands that session the right playbook: read the compact scoreboard (never the
worker firehose, which would blow up your token bill), the steering vocabulary, and what to watch for.
And because a Claude Code session is reachable from the mobile app, you can check in and
course-correct from your phone:

```
/agg:supervise
> how's it going?
> inject: the auth refactor is the blocker — do that first
```

The supervisor reads only `agg/private/state.json` — the small scoreboard snapshot — and `agg status`. It
**never** tails the workers' output, so supervising a long run costs you almost nothing.

## Choosing an agent

```yaml
# agg/agg.yaml
defaults:
  agent: "claude"   # claude (default) · codex · copilot — the worker default
```
Supported features:

| | Claude | Codex | Copilot |
|---|---|---|---|
| Runs the loop, edits files | ✅ | ✅ | ✅ |
| Script judges (a `.sh` file) | ✅ | ✅ | ✅ |
| **LLM judges** (a `.md` rubric file) | ✅ | ✅ | ✅ |
| **Progress summaries** (`summary.enabled`) | ✅ | ✅ | ✅ |
| Token budget (`sequence.limits.tokens`, `over_budget`) | ✅ | ✅ | ✅ |
| Thinking effort (`effort:`) | ✅ | ✅ | ⚠️ ¹ |
| Rate-limit backoff | ✅ | ✅ | ❌ |
| **Dollar cost cap** (`sequence.limits.cost`, `over_cost`) | ✅ | ❌ | ❌ |

- **`model:`** — **Codex: omit it entirely.** Which models you may use depends on how you
  authenticated, and naming a wrong one is a hard 400 at runtime. **Copilot: `model: auto`.**
- **`effort:`** takes `low|medium|high|xhigh|max`. Codex tops out at `high` (`max` clamps to it).
  ¹ **Copilot rejects `effort:` together with `model: auto`** — which is its default. Name a concrete
  model, or leave `effort:` empty. `agg` refuses the pair at startup rather than letting every
  session die.
- **Dollar cost** is Claude-only: Codex reports no cost at all, Copilot bills in AI Credits, not
  dollars. Ask for `sequence.limits.cost` / `over_cost` on either and `agg run` **refuses to start** — a
  spend guard that can never fire is worse than none. This is checked **per step**: even one
  `agent: codex` step in an otherwise-Claude sequence makes the cost guard uncoverable. Use
  `sequence.limits.tokens` (tokens), which works everywhere. Copilot can also cap itself:
  `worker_args: ["--max-ai-credits", "50"]`.

**Where `agg skills install` puts the skills.** Both routes — the plugin marketplace (see
[Quick start](#quick-start)) and `agg skills install` — work on all three agents. The *only* per-agent
difference is the install directory, which is why the latter is a command and not a `cp`:

| agent | project install | `--user` install |
|---|---|---|
| Claude Code | `.claude/skills/` | `~/.claude/skills/` |
| OpenAI Codex · GitHub Copilot | `.agents/skills/` | `~/.agents/skills/` |

Codex and Copilot both honour the agent-neutral `.agents/` convention, so one directory serves both;
Claude reads neither.

## Features

The high-level capabilities at a glance — deeper detail lives in the linked sections and
[`docs/CONFIG.md`](CONFIG.md).

**Correctness — the moat**
- **Deterministic four-stage loop** — INJECT → RUN → VERIFY → GATE; only RUN is stochastic.
- **Fresh session every iteration** — no context degradation; git + memory carry state, not a long chat.
- **Incorruptible judges** — agg runs them, the agent never grades itself ([script or LLM-as-judge](#building-judges)).
- **Judges-as-DoD** — compose judge names with `AND` / `OR` / `NOT` (plus numeric accessors like `coverage.value >= 80`) into one `done_if` expression; resolved by name from disk, no registry.
- **Per-step agents + sequences** — a repeating list of steps, each with its own agent/model, so a stall can step back on a *different vendor* ([Steps and sequences](#steps-and-sequences)). Making that step-back **conditional** ("only when stalled") is the [Rust driver API](#when-yaml-is-not-enough-the-rust-driver-api) — YAML has no `if:`.
- **Post-merge rollback gate** — a session that regresses a previously-met judge is reverted; the base never advances broken.
- **A Rust driver API, when the flow is a program** — `agg.step()` / `agg.judge()` / `agg.gate()` from ordinary Rust, for control flow `agg.yaml` cannot express: branch on a verdict's number, step back only when stalled, or short-circuit a 40-minute judge behind three cheap ones with `&&`. Same engine, same judges — heavier, and Rust-only, so **use YAML unless you need it** ([when YAML is not enough](#when-yaml-is-not-enough-the-rust-driver-api)).
- **Crash-resumable driver runs** — every completed `agg.*` call is recorded, so a driver that dies at hour six replays its finished work in seconds and spends nothing on it. Your loop is never serialized: it runs from the top and its own `if`/`for` walk it back. Fast-forward reaches back to the last gate that landed on base — never past it, because staged work lives on a branch the ledger cannot describe ([details](#when-yaml-is-not-enough-the-rust-driver-api)).

**Guardrails for unattended runs**
- **Blast-radius isolation** *(`isolation: sandbox | container`, per step)* — confines what an auto-mode worker (and its judges + hooks) can do to the **host** (`rm -rf ~`, read `~/.ssh`), orthogonal to session isolation. Two tiers: **`sandbox`** — an OS jail (`sandbox-exec` on macOS, `bwrap` on Linux) limiting **writes** to the project dir + tmp, reads and network open; **`container`** — re-hosts the worker inside a **Docker/Podman** image (`image:` key) with the project dir bind-mounted, the container boundary as the jail. On both tiers `agg/private/` is carved back **out** of the writable set, so a confined worker cannot forge the verdict ledger that decides when its own run ends. ⚠ **A JUDGE is confined more tightly than the worker**: the project tree is **read-only** to it — a judge able to write what it grades can edit its way to a pass — and its writes are *relocated* to `$AGG_JUDGE_SCRATCH` plus a persistent toolchain cache (`CARGO_TARGET_DIR`, `PYTHONPYCACHEPREFIX`, …), so a well-behaved judge needs no change ([details](CONFIG.md)). Refused at startup if the mechanism is missing, never a silent downgrade ([details](CONFIG.md)). *macOS verified; Linux experimental.*
- **Rate-limit backoff** *(Claude + Codex)* — detects a usage/429 limit, discards the incomplete session, waits, and retries fresh.
- **Stall watchdog** — kills a worker that's gone both idle *and* CPU-flat.
- **Stuck detection + async human notification** *(`sequence.notify_if` + `notify`)* — the non-terminal twin of `abort_if`: when a detector fires, `agg` runs your `notify.cmd` (a push, a webhook, a log line) and **the loop keeps running**. Detectors are just judges — the shipped `stalled`, or a `stuck` rubric you write over the verdict history — so there's no new machinery, and a human stays a *side-channel*, never a gate. Debounced by `notify.cooldown_sessions` ([details](CONFIG.md)).
- **No orphaned compute** — process-group reaping sweeps stragglers when a session or the loop ends.
- **Token ceilings on every agent, dollar ceilings on Claude** — `over_budget` / `over_iterations` / `wall_hours` everywhere; `over_cost` only where the agent reports dollars. Asking for a guard your agent can't report is refused at startup, never silently ignored.
- **Long-task tracking** — `agg spawn` keeps a sim/build alive across sessions so the reaper spares it.

**State & memory**
- **Cross-session memory** — durable `LOG.md`, its recent tail injected into every session.
- **Plain-file state** — crash-safe and observable; no database or daemon ([details](#state-and-memory)).
- **Rolling summaries** — cheap progress digests for the dashboard and supervisor.

**Control & observability**
- **Live TUI + standalone web UI** — same state, two views ([Interfaces](#interfaces)).
- **Chat supervisor** (`/agg:supervise` · `/agg-supervise` · `$agg-supervise`) — status and steering by chat in a second agent session; from your phone via Claude Code's mobile app.
- **Session-granular steering** — `agg send inject / budget / pause / resume / stop`.
- **Automation-friendly** — `--json` output and meaningful exit codes.

**Works with your setup**
- **Claude Code, OpenAI Codex or GitHub Copilot** — subscription or API key; the worker inherits your agent's MCP servers, plugins, hooks, and skills.
- **Tool-agnostic hooks** — wire in your own code graph, linter, or memory via lifecycle hooks + prompt includes.

## Steering a running loop

You can't interrupt a headless agent mid-thought (a platform limit), so steering is
**session-granular**: queued on a bus, applied at the next `INJECT`.

```bash
agg send inject "focus on the auth module; it's the blocker"
agg send budget 8000000        # change the token ceiling mid-run
agg send pause                 # …and `agg send resume`
agg stop "done for today"      # graceful stop at the next GATE
```

Or skip the exact commands: tell your `/agg:supervise` session in plain English — *"inject: focus on
the auth module", "raise the budget to 8M", "pause for now"* — and it runs the right `agg send` for
you.

`Ctrl-C` or `agg stop` shuts the agent and the loop down cleanly — no orphaned worker, ledger
finalized, base branch untouched.

## Building judges

A judge is any command that prints a **verdict** as JSON to stdout. Only `met` is required; the rest
are optional:

```jsonc
{
  "met":       true | false,   // required — did this goal pass?
  "value":     <number>,       // optional — a count or percent (drives the progress bar)
  "max":       <number>,       // optional — the denominator for value
  "target":    <number>,       // optional — the value that counts as "done"
  "rationale": "<one line>",   // optional — shown on the dashboard
  "evidence":  ["<line>", ...] // optional
}
```

So a **binary** judge can print just `{"met": true}`; a numeric one might print, as **one** example:

```json
{"met": false, "value": 18, "max": 28, "target": 28, "rationale": "18/28 tests pass"}
```

`agg` uses the last JSON object on stdout, so a judge can log freely and print its verdict last. There
is no `type:` field — a judge that emits a `value` shows partial progress; one that emits only `met`
is binary.

**Two flavours, chosen by extension.** A judge is a **file whose name is the judge name**, resolved
from `agg/judges/<name>.{sh,md}`, then the standard library at `~/.agg/judges/`:

- **`.sh` → a script judge.** Its stdout is the verdict JSON. It runs from the project root with
  `AGG_SESSION` / `AGG_STEP` / `AGG_JUDGE` / `AGG_PROJECT_DIR` in the env (a script judge can even
  shell out to `claude -p` itself).
- **`.md` → an LLM judge, and the file IS the rubric.** It declares the files it reads in its own YAML
  **frontmatter** (`inputs: ["diff", "src/main.rs"]`) — one self-contained file, no `kind:` tag, no
  registry. `agg` builds the prompt from your rubric plus those inputs (wrapped so the repo content is
  fed as *untrusted data* the judge is told never to obey), runs it **isolated** on the RULER (the
  `judge:` block) — `--setting-sources user` + `--strict-mcp-config`, no `--dangerously-skip-permissions`
  — and extracts the verdict. The judge model is the RULER's, set once for the whole run.

Ready-made **parameterless** judges ship inside the binary and install to `~/.agg/judges/`
(`cargo_test`, `build_ok`, `lint_clean`, `git_clean`, `no_regression`, `stalled`, `cmd_exit`,
`grep_count`) — name any of them in a condition and it just resolves. Copy one into `agg/judges/` to
shadow it by name. The source lives in [`plugin/judges/`](../plugin/judges/) + [`plugin/rubrics/`](../plugin/rubrics/).

**Compose judges into your DoD.** Each judge checks **one clause**; `done_if` composes them into the
whole of it — any boolean of judge names (`a AND b`, `a OR b`, `NOT c`, with parentheses), plus numeric
accessors (`coverage.value >= 80`) and aggregates (`met_fraction >= 0.75`, `count_met >= 3`). So
`done_if` *is* your DoD, as an expression.

`abort_if` takes the same grammar but is **not** part of your DoD: it is a ceiling (budget, time,
a regressed invariant, a judge error) that aborts the run as a failure. Done is one thing; giving up
is another:

`notify_if` takes the same grammar again, and is the one clause that **doesn't end the run**: when it's
true, `agg` runs `notify.cmd` and the loop keeps going. Done is one thing, giving up is another —
*asking for help* is a third, and it must not stop the loop to do it:

```yaml
sequence:
  done_if: "outputs_two AND tests_pass"                  # both must hold — exit 0
  abort_if: "any_regressed(invariants) OR over_budget"   # optional guard — exit 3 — see docs/CONFIG.md
  notify_if: "stalled"                                   # optional PING — the loop KEEPS RUNNING
  notify:
    cooldown_sessions: 5                                 # at most one ping per 5 sessions (default 3)
    cmd: ["curl -s --max-time 10 -d {{reason}} ntfy.sh/my-topic"]   # bound it: delivery is FOREGROUND
```

`{{reason}}` is the rationale of a judge named in the expression (`met` first, then highest `value`) —
a heuristic, not proof of which subterm fired. It, plus `{{project}}` / `{{session}}` / `{{step}}`, is
substituted **shell-quoted by `agg`** — so never wrap a placeholder in quotes of your own. Which clause
a detector sits in *is* your human policy: none = pure autonomy, `notify_if` = tell me but keep going,
`abort_if` = stop. Put
**worker-authored** signals in `notify_if` only, or the agent gains a way to end its own run — the
tradeoff, and the copy-ready `stuck` / `blocked` detectors, are in [`docs/CONFIG.md`](CONFIG.md).

The rule that makes it a moat: **`agg` runs the judges, never the agent.** So make the judges check
the resulting artifacts — tests, a compiler, a proof checker — not the agent's claim about it — the agent will often enough hallucinate that a job is done.

## Steps and sequences

`done_if` says *what done means*. A **sequence** says *how the loop spends its sessions getting
there*. Most projects need one plain step; the power shows up on long, open-ended runs.

A **step** is an `(agent, model, role)` triple — a body of overrides over `defaults:`. A **sequence**
is a repeating list of entries over those steps:

```yaml
steps:
  worker: {}                       # pure defaults — the grunt worker
  reconsider:
    agent: "codex"                 # a DIFFERENT vendor — the perspective diversity is the point
                                   # (no model: — Codex picks its own; naming one is a hard 400)
    prompt: >
      Assume the current approach is wrong. Name 2-3 fundamentally different approaches,
      pick one, and write the rejected ones and WHY into your scratch note.
    skip_judges: true              # no DoD judges run → nothing merges; this step's work STAGES

sequence:
  steps:
    - { step: worker, until: "NOT stalled", max: 4 }  # repeat while stalled, at most 4 sessions
    - reconsider                   # step back on a different vendor — runs EVERY lap, see below
```

An entry is tiny: a bare `NAME`, or `{ step: NAME, times: N }`, or `{ step: NAME, until: <expr>,
max: N }` (the `<expr>` is the same `done_if` grammar, checked after each dispatch). Both bounds are
mandatory. The list repeats from the top until `done_if` or `abort_if` fires.

⚠ **Two things about `until:` that decide whether a sequence does what you think.**

**There is no `if:`** — a lap dispatches *every* entry, in order. `reconsider` above runs on every
lap, not only after a stall. YAML cannot say "step back **only when** stalled"; that is conditional
dispatch, and it belongs to the [Rust driver API](RUST_API.md), where it is an ordinary `if`.

**`max:` is a contract, not a budget.** An entry that spends its whole bound with the condition still
false has failed, and the run **aborts** naming the bound and the condition — it does not quietly
advance. Exhaustion and convergence used to take the same branch, which meant a step burning four
sessions against a judge that was *timing out* looked exactly like a step that had succeeded. Pick
these numbers as the point at which a human should be told, because that is what they now mean.

**Why per-step *agents* is the headline, not a bonus.** The clearest finding in the reflection
literature is that an agent reflecting on its own reasoning tends to *reinforce* the flaw rather than
escape it — unless the reflection comes from a **different perspective**. A different **vendor** is the
strongest diversity available (different training, priors, failure modes): **a Claude rabbit hole is
not necessarily a Codex rabbit hole.** So a stall-triggered `reconsider` step on a *different agent* is
a sharper instrument than "a stronger model of the same agent". And because `agg` already resets
context every session and carries state in git + memory (both agent-agnostic), swapping vendor between
sessions **costs nothing** — there is no conversation to hand over.

**Plus cost.** Most sessions in a long run are grunt work — run them on a cheap model/agent, and spend
the strong one only on the step-back. The repeated `worker` on a cheap agent, `reconsider` on the
expensive one.

The `stalled` builtin (a library judge over the verdict history) is what BOUNDS the grunt repeat —
it does not *trigger* the next entry, since every entry runs every lap; a
`skip_judges: true` step **stages** its work rather than merging it, and the next judged step gates the
whole span — so a red-team detour can never get the *next* session rolled back in its place. See
[`examples/p-vs-np/`](../examples/p-vs-np/) for a full sequence, and [`docs/CONFIG.md`](CONFIG.md)
for the complete grammar.

## When YAML is not enough: the Rust driver API

**Heavier and Rust-only — use `agg.yaml` unless you need flow it cannot express.** The section above
is the honest boundary: a YAML sequence is a list that laps, with bounded repetition. Four things it
deliberately cannot do, each of which is an `if` or a `for` in Rust:

| you want | YAML | Rust |
|---|---|---|
| skip a step based on a verdict | ✗ — a lap runs every entry | `if agg.judge(&x).met() { continue }` |
| **not run** an expensive judge unless cheap ones pass | ✗ — every run-set judge runs after every judged step | `agg.judge(&cheap).met() && agg.judge(&slow).met()` |
| a bound that changes with what the run learned | ✗ — `max:` is a literal | any expression |
| state that changes *within* a cycle | ✗ — verdict rows land per gate | a local variable |

The second row is the one that costs money: a 40-minute load test in a YAML run-set executes after
**every** judged step; in a driver, `&&` short-circuits and it runs only where it matters.

It is the **same engine** — `agg.step()` dispatches through the identical primitive the YAML walk
does, so session isolation, the keep/rollback gate, memory, judges and the ledger all behave
identically. What you gain is ordinary Rust; what you pay is a compiled binary instead of a config
file.

```rust
let agg = Agg::open(".")?
    .limits(Limits { tokens: Some(40_000_000), cost: None,
                     sessions: Some(300), wall_hours: Some(10.0) })
    .on_regression(OnRegression::Rollback);

let cycle = agg.pos("cycle", 20);              // the breadcrumb the TUI renders
for c in 1..=20 {
    cycle.update(c);
    agg.check_limits()?;                       // ceilings are OPT-IN — they fire here

    agg.step(&implement)?;                     // stages on a session branch; nothing merged yet
    if !(agg.judge(&tests).met() && agg.judge(&load).met()) {
        continue;                              // `&&` IS the cost gate: `load` never runs when red
    }
    agg.gate()?;                               // land the whole span, or discard it per policy
}
```

Eleven calls in total. **`step()` stages, `gate()` lands** — a driver that never gates loses nothing
(every session is committed on its own branch) but never advances the base branch either. Judges are
**lazy and memoized per step**, which is what makes `&&` a real cost gate rather than a style choice.

A crashed run resumes with `Agg::open_with(".", Opts { resume: true })`: completed calls are answered
from `agg/private/calls.jsonl` — no worker, no ruler, no tokens — back as far as the last gate that
landed. Your loop is never serialized; it runs from the top and its own control flow walks it back.

Full surface, the resume rules, and the three things a driver author must design around:
[`docs/RUST_API.md`](RUST_API.md). Working drivers: [`examples/workflow.rs`](../examples/workflow.rs)
and [`examples/selfimprove.rs`](../examples/selfimprove.rs), with
[`examples/workflow.yaml`](../examples/workflow.yaml) as the same workflow in YAML plus an honest list
of what it loses.

## State and memory

`agg` keeps **no state in a database or a long-running daemon**. Config lives under `<project>/agg/`
(committed: `agg.yaml`, `AGG.md`, judges); runtime state lives in **two** gitignored, auto-created
directories beside it — plus git itself. The loop is the single *writer* of the private half; every
*reader* (the TUI, the web UI, `/agg:supervise`, `agg status`) reads the same files, so you can attach
any number of views to a running loop without coupling any of them to it.

The two directories are split by **who may write them** — *if the worker writing it could change when
the loop ends, what it may spend, or what agg believes happened, it is private:*

```text
<project>/agg/
  agg.yaml · AGG.md · judges/     COMMITTED — your config, your graders
  state/                          GITIGNORED — the WORKER writes it (agg reads it as untrusted input)
    STATE.md  wiki/  sessions/  spawns.json  spawns/  BLOCKED.md
  private/                        GITIGNORED — AGG writes it; a CONFINED worker cannot
    INSTRUCTIONS.md  LOG.md  state.json  project.json  verdicts.jsonl  bus/  run.pid  run.log
```

- **`agg/private/state.json`** — the live scoreboard snapshot (judges, the current step + its agent,
  tokens, phase, activity tail), written atomically after each change. The TUI, `agg serve`, and the
  supervisor read this.
- **`agg/private/verdicts.jsonl`** — the append-only, safety-critical GATE record: one line per verdict,
  stamped with its merge outcome. The gate's "was previously met" and `stalled` both read it.
- **`agg/private/run.pid`** · **`agg/private/run.log`** — the loop's liveness (double-run guard, `agg stop`
  target) and its log when detached.
- **`agg/private/bus/`** — the steering queue: `agg send …` writes a command here; the loop drains it at
  the next `INJECT`. Private because it is the *operator's* channel — `agg send budget` would otherwise
  let a worker raise its own token ceiling.
- **`agg/private/project.json`** — the run-history ledger (`agg history`): lifetime sessions/tokens across runs.
- **`agg/AGG.md`** (committed) — the stable scope/goals/architecture the worker reads for
  orientation; human-owned, rarely edited. **`agg/state/STATE.md`** — the forward "what to do next"
  advice the **worker** rewrites each session; gitignored, so it survives a session rollback (the code
  attempt is thrown away, the advice about it is not).
- **`agg/private/INSTRUCTIONS.md`** — regenerated by **agg** every session and pointed at by the worker's
  entire `-p`; agg composes it from the step's role + `prompt:`, a recent tail of memory, any queued
  steering, and pointers to `AGG.md` and `STATE.md`. Disposable; the worker reads it and never edits it —
  it is the worker's *orders*, so rewriting it would launder instructions past you.
- **`agg/private/LOG.md`** — durable cross-session memory (`what we tried and rejected`), written by
  **agg** (never the worker); its recent tail is injected into every session. The worker's own
  contribution arrives as a scratch note in `agg/state/sessions/` that agg sanitizes on the way in.
- **git commits** — the actual *work* state. **agg** commits each session's work on its own branch (the
  worker never runs git) and gates it; the next **fresh** session resumes from the filesystem + `LOG.md`.
  Git *is* the memory between sessions.

**`agg/private/` is carved out of the sandbox's writable set.** Under `isolation: sandbox` or
`container`, the worker may write the project dir and its own `agg/state/` but writes to `agg/private/`
are refused by the kernel — otherwise a confined worker could append forged `merged` rows to
`verdicts.jsonl`, make `stalled` report met, and end its own run under `abort_if: "stalled"`. **Reads
are untouched**: the worker still reads its brief and judges still read the ledger. Under the default
`isolation: none` the worker has your whole filesystem and no directory layout changes that — see
[isolation](CONFIG.md#isolation--blast-radius-jail-a-different-axis-from-session-isolation).

> **Upgrading an existing project?** The private files used to live in `agg/state/`. Nothing there is
> tracked, so the migration is a `mv` — see
> [Migrating an existing project](CONFIG.md#migrating-an-existing-project).

Because the log on stdout is the source of truth and `state.json` is just a view of it, a run is
**crash-safe and observable**: `tail`/`grep`/`cat` the files, kill and restart, inspect mid-run —
nothing important is hidden in process memory.

## Your agent's setup carries in

`agg` bundles no tools of its own. `RUN` is an ordinary `claude -p` / `codex exec` / `copilot -p`
call, so it **inherits your agent's environment** — MCP servers, plugins, settings, and hooks all keep
working inside the worker, and `/agg:new` will even detect your active MCP servers / skills / hooks
and offer to wire them into the loop (via `hooks:` + `prompt_includes:`). Nothing is hardcoded;
whatever your agent has, the worker gets.

Running an agent headlessly does impose real limits — worth knowing up front:

- **Don't rely on invoking a skill by name** inside the worker. Inline the content you need into
  `agg/AGG.md` rather than calling `/some-skill`.
- **No mid-session interruption** (a platform limit on all three), so steering is session-granular —
  queued on the bus, applied at the next `INJECT`.
- **The worker runs with full tool access**, because a headless agent can't answer permission
  prompts: `--dangerously-skip-permissions` (Claude), `--dangerously-bypass-approvals-and-sandbox`
  (Codex), `--allow-all-tools` (Copilot). Narrow it with `worker_args`, which passes flags **verbatim
  to your agent** — so use that agent's own vocabulary (e.g. Claude's `--allowedTools Edit,Bash`,
  Copilot's `--max-ai-credits 50`).
- **MCP servers that need interactive auth** (an OAuth login, say) may be unavailable in a headless run.
- The **judge & summarizer** sessions are deliberately **read-only**, so the agent can't steer the
  process that grades it. Same guarantee, three mechanisms: Claude `--strict-mcp-config` +
  `--setting-sources user`; Codex `--sandbox read-only`; Copilot by withholding `--allow-all-tools`.

## Interfaces

Two ways to watch a run — same live state, two views — plus a chat supervisor:

<p align="center">
  <img src="assets/dashboard.png" alt="agg dashboard — the live TUI scoreboard, goals, and activity tail" width="49%">
  <img src="assets/web.png" alt="the agg web UI — scoreboard, controls, and activity log" width="49%">
</p>
<p align="center">
  <sub><b>TUI</b> (<code>agg dashboard</code>) &nbsp;·&nbsp; <b>Web</b> (<code>agg serve</code> + the SvelteKit app in <code>src/web/</code>)</sub>
</p>

| | |
|---|---|
| **TUI** | `agg dashboard` — live scoreboard, goals, activity tail. `Tab` focus · `↑↓` `PgUp` `PgDn` `g` `G` scroll · `f` follow · `q` quit. `--once` prints a one-shot snapshot for CI/SSH. |
| **Web** | A standalone SvelteKit app in [`src/web/`](../src/web/). The binary stays UI-free and exposes a thin JSON API. |
| **Supervisor** | `/agg:supervise` (Claude) · `/agg-supervise` (Copilot) · `$agg-supervise` (Codex), in a second agent session — status and steering by chat; a Claude session is reachable from the mobile app. Reads the snapshot only, not the workers' output. |

```bash
agg serve                              # JSON API on :7878
cd src/web && npm install && npm run dev   # the web UI on :5173
```

## CLI reference

Global flags, valid on every subcommand: `--dir <path>` (project root, default `.`),
`--config <file>` (default `<dir>/agg/agg.yaml`).

| Command | What it does | Flags |
|---|---|---|
| `agg init` | Scaffold **placeholder** config you must then edit — a blank-slate fallback. Prefer `/agg:new`, which fills it in from your project. Writes `agg/agg.yaml` + `agg/AGG.md` + a starter judge. | `--agent <a>` shape it for that agent · `--force` overwrite |
| `agg doctor` | Diagnose the setup: **every agent the sequence names** is on PATH, config parses, conditions valid, judges resolve, **the agents can do what the config asks**, skills installed | |
| `agg skills install` | Install the `agg-*` skills where your agent looks (`.claude/skills/` for Claude, `.agents/skills/` for Codex + Copilot) | `--agent <a>` (default: agg.yaml's `agent:`) · `--user` install under `$HOME` |
| `agg plan` | Run every run-set judge once and print the starting scoreboard (a dry run) | |
| `agg run` | Drive the loop until `done_if` is met (or a guard fires) | `--max-sessions <n>` (0 = unlimited) · `--detach` / `-d` |
| `agg judge <name>` | Resolve **one** judge by name and print its raw verdict — for authoring judges | |
| `agg status` | The loop's latest scoreboard, from its snapshot (cheap; re-runs no judges) | `--json` |
| `agg history` | This project's run history, newest first, plus lifetime totals | `--json` |
| `agg dashboard` | Live TUI | `--once` one-shot text snapshot |
| `agg serve` | JSON API for the web UI: `/api/state`, `/api/history`, `/api/health`, `POST /api/send` | `--port <n>` (7878) · `--cors-origin <url>` · `--token <t>` |
| `agg spawn` | *(used by the worker, not to start the loop)* track a long child task so the reaper spares it and the next session polls it | `--name <n>` · `--reason <why>` · `-- <cmd…>` |
| `agg stop [reason]` | Graceful stop at the next session boundary (the one top-level steering alias) | |
| `agg send <cmd>` | All steering, applied at the next session boundary | `inject <text>` · `budget [total]` · `pause` · `resume` · `stop [reason]` · `note <text>` |

`agg run` exit codes, so automation can branch on the outcome: **0** `done_if` met (or an operator
stop) · **1** hard error · **3** a guard fired (`abort_if`) · **4** hit `--max-sessions`.

## Configuration

You don't normally write this by hand — `/agg:new` generates `agg/agg.yaml` (and the judge files
under `agg/judges/`) for your project, and `agg run` reads `agg/agg.yaml` by default. When you want to
tune it: the `agent:` (see [Choosing an agent](#choosing-an-agent)), the `steps` / `sequence` model,
token and dollar ceilings, the rolling summary, cross-session memory, mandatory per-session git
isolation with a rollback gate, lifecycle hooks, watchdog thresholds, judges-by-name resolution, and
the `done_if` / `abort_if` / `notify_if` grammar are all documented in
**[`docs/CONFIG.md`](CONFIG.md)**.

## Examples

- **[`examples/hello-agg/`](../examples/hello-agg/)** — the smallest possible loop, runnable in a
  minute, plus a walkthrough that drives a project to "all tests pass".
- **[`examples/p-vs-np/`](../examples/p-vs-np/)** — the showcase: every feature aimed at one famous
  problem, with a Lean proof checker as the incorruptible judge.
