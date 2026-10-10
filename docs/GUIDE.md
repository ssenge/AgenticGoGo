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
run it before your first loop. `agg check` validates the config alone and lists every problem at
once; both take `--json` for scripts. See [Choosing an agent](#choosing-an-agent) for what each one
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
  abort_if: "over_iterations OR work_time >= 3600"   # a ceiling, not part of the DoD
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

**Another agent CLI?** A backend is one Rust module behind the `AgentBackend` trait; how to write one,
with the capability flags, the recorded fixtures and the required tests, is in
[docs/BACKENDS.md](BACKENDS.md). The Kiro CLI was investigated and is not supported yet:
[why](backends/kiro.md).

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
- **Numeric ratchet** *(`sequence.ratchet`)* — a tracked number (complexity, coverage, latency) may not get worse than at the last kept gate beyond a tolerance, so the agent cannot trade one metric for another; seed the first reference or let the baseline pass measure it ([details](CONFIG.md#the-ratchet--numeric-no-regression)).
- **Judge groups, k of n** *(`sequence.groups`)* — name several judges that measure one thing (three static-analysis tools that disagree) and count over just them: `count_met(static_quality) >= 2`, `all_goals(security)`. At the gate a `done_if` group is judged net, so one tool flipping while another catches up is not a regression ([details](CONFIG.md#judge-groups--k-of-n)).
- **Judges built on swe-metrics** *(`judges.<name>.metric` / `.score`, optional)* — declare a judge as one number of a [swe-metrics](https://github.com/ssenge/swe-metrics) report (cyclomatic complexity p95 from lizard, duplication %, critical+high vulnerabilities) or as its 0–100 quality score, with a `max:`/`min:` threshold — no script to write. Every such judge of a session shares **one** measurement; a skipped or failed analyzer makes the judge error, never pass; several tools are merged with `combine: strictest | all | majority`; `scope: changed` judges only new code (and is vacuously met when there is none); the score is computed against the incumbent's report. Works with the ratchet without a `direction:`. agg works without swe-metrics; declaring such a judge without it is refused at startup — checked where it will run: the host, or `judge.image` on a container run, which needs no swe-metrics on the host (reference image `docker/swe-metrics`) ([details](CONFIG.md#judges-built-on-swe-metrics--metric-and-score)).
- **Merge only on improvement** *(`sequence.merge_requires_improvement`, opt-in)* — a session that regressed nothing but improved nothing either (a reformat) is not merged and is recorded `no_improvement`; not a rollback, but it counts for `stalled` and `consecutive_no_improvements` ([details](CONFIG.md#merge-only-on-improvement--merge_requires_improvement)).
- **Damage and oscillation stops** — `rollbacks`, `consecutive_rollbacks` and `oscillating` (fix A, break B, fix B, break A) are built-in condition terms computed by the gate itself, so they are unfakeable and safe in `abort_if` ([details](CONFIG.md#rollbacks-and-oscillation--the-gates-own-detectors)).
- **A Rust driver API, when the flow is a program** — `agg.step()` / `agg.judge()` / `agg.gate()` from ordinary Rust, for control flow `agg.yaml` cannot express: branch on a verdict's number, step back only when stalled, or short-circuit a 40-minute judge behind three cheap ones with `&&`. Same engine, same judges — heavier, and Rust-only, so **use YAML unless you need it** ([when YAML is not enough](#when-yaml-is-not-enough-the-rust-driver-api)).
- **Crash-resumable driver runs** — every completed `agg.*` call is recorded, so a driver that dies at hour six replays its finished work in seconds and spends nothing on it. Your loop is never serialized: it runs from the top and its own `if`/`for` walk it back. Fast-forward reaches back to the last gate that landed on base — never past it, because staged work lives on a branch the ledger cannot describe ([details](#when-yaml-is-not-enough-the-rust-driver-api)).

**Guardrails for unattended runs**
- **Blast-radius isolation** *(`isolation: sandbox | container`, per step)* — confines what an auto-mode worker (and its judges + hooks) can do to the **host** (`rm -rf ~`, read `~/.ssh`), orthogonal to session isolation. Two tiers: **`sandbox`** — an OS jail (`sandbox-exec` on macOS, `bwrap` on Linux) limiting **writes** to the project dir + tmp, reads and network open; **`container`** — re-hosts the worker inside a **Docker/Podman** image (`image:` key) with the project dir bind-mounted, the container boundary as the jail — the agent CLI itself runs in there (reference images in `docker/`, credentials passed by name, never baked in), and so do its judges. On both tiers `agg/private/` is carved back **out** of the writable set, so a confined worker cannot forge the verdict ledger that decides when its own run ends. ⚠ **A JUDGE is confined more tightly than the worker**: the project tree is **read-only** to it — a judge able to write what it grades can edit its way to a pass — and its writes are *relocated* to `$AGG_JUDGE_SCRATCH` plus a persistent toolchain cache (`CARGO_TARGET_DIR`, `PYTHONPYCACHEPREFIX`, …), so a well-behaved judge needs no change ([details](CONFIG.md)). Refused at startup if the mechanism is missing, never a silent downgrade ([details](CONFIG.md)). *macOS verified; Linux experimental.*
- **Read-only context** *(`context:`)* — directories from **outside** the project (a knowledge base, reference architectures) that every session reads and none may change: kernel-denied under `sandbox`, mounted `:ro` at `/context/<as>` under `container`, listed in the brief with a pointer to each `index.md` ([details](CONFIG.md#context-directories--context-read-only-material-from-outside-the-project)).
- **Network egress allow-list** *(`isolation_policy.network: { allow: [...] }`)* — the worker and its judges reach only the listed hosts, through agg's own proxy: an `--internal` network under `container` (the reference), Seatbelt under macOS `sandbox`; refused where it cannot be enforced, and refused when the agent's own endpoint is missing from the list ([platform matrix](CONFIG.md#network-egress--isolation_policynetwork)).
- **Protected paths** *(`write_deny`, in `defaults` and per step)* — globs the worker may **read but never change**: oracles, golden files, rule files, gate config. An agent that cannot pass a test will edit the test; this is what stops it. Enforced on **every** tier, `none` included, by a content-hash check at the gate over every covered file — tracked, untracked and gitignored: a session that changed one is **rolled back**, the files are restored from agg's own copies before any judge runs, and a `protected_path_violation` row with the paths lands in `agg/private/violations.jsonl`. Under `sandbox`/`container` the kernel also denies the write itself. A step's list is added to the default, never subtracted from. Tested against a fake agent that tries direct writes, symlinks, hard links, renames, `chmod`, `git checkout`, case variants and more ([details + platform matrix](CONFIG.md#protected-paths--write_deny-readonly-writable)).
- **Rate-limit wait** *(Claude + Codex)* — detects a usage/429 limit, saves the incomplete session's work under `refs/agg/partial/<run>/<session>` (never judged), waits for the reset and retries fresh. The wait shows in `agg status` (`waiting: {reason, agent, until}`), `agg send wake` ends it early, it never counts against `work_time`, and `agg run --resume` continues an interrupted run ([details](CONFIG.md#rate-limits--wait-wake-resume)).
- **Stall watchdog** — kills a worker that's gone both idle *and* CPU-flat.
- **Stuck detection + async human notification** *(`sequence.notify_if` + `notify`)* — the non-terminal twin of `abort_if`: when a detector fires, `agg` runs your `notify.cmd` (a push, a webhook, a log line) and **the loop keeps running**. Detectors are just judges — the shipped `stalled`, or a `stuck` rubric you write over the verdict history — so there's no new machinery, and a human stays a *side-channel*, never a gate. Debounced by `notify.cooldown_sessions` ([details](CONFIG.md)).
- **No orphaned compute** — process-group reaping sweeps stragglers when a session or the loop ends.
- **Token ceilings on every agent, dollar ceilings on Claude** — `over_budget` / `over_iterations` / `wall_time`/`work_time` everywhere; `over_cost` only where the agent reports dollars. Asking for a guard your agent can't report is refused at startup, never silently ignored.
- **Long-task tracking** — `agg spawn` keeps a sim/build alive across sessions so the reaper spares it.

**State & memory**
- **Cross-session memory** — durable `LOG.md`, its recent tail injected into every session.
- **Plain-file state** — crash-safe and observable; no database or daemon ([details](#state-and-memory)).
- **Rolling summaries** — cheap progress digests for the dashboard and supervisor.

**Control & observability**
- **Live TUI + standalone web UI** — same state, two views ([Interfaces](#interfaces)).
- **Chat supervisor** (`/agg:supervise` · `/agg-supervise` · `$agg-supervise`) — status and steering by chat in a second agent session; from your phone via Claude Code's mobile app.
- **Session-granular steering** — `agg send inject / budget / pause / resume / wake / stop`.
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
agg send wake                  # end a rate-limit wait now (≥ 60 s after the limited attempt)
agg stop "done for today"      # graceful stop at the next GATE
```

A **rate-limit wait** is the one place a command acts between sessions: the waiting loop polls the
bus every second, so `wake` (or `resume`) starts the next attempt and `stop` ends the run (exit 5) right
away. If the agent is still limited, agg notices and waits again. An interrupted run — Ctrl-C, `agg
stop`, a crash — continues with `agg run --resume`: same run id, the next session number, back to its
last kept gate; the cut-off session's work waits on `refs/agg/partial/<run>/<session>`.

⚠ **Every `agg send` needs a workflow running** — it is refused otherwise, naming the prerequisite.
A steering message with nothing to steer is not queued, it is a landmine that fires at the startup of
whatever runs next. Stale commands are purged when a workflow starts. (Answering a human ask is
different and needs no running workflow — it is `agg answer`, not a `send`.)

Or skip the exact commands: tell your `/agg:supervise` session in plain English — *"inject: focus on
the auth module", "raise the budget to 8M", "pause for now"* — and it runs the right `agg send` for
you.

`Ctrl-C` or `agg stop` shuts the agent and the loop down cleanly — no orphaned worker, ledger
finalized, base branch untouched.

## When a human is unavoidable

Steering above is *you* interrupting the loop. This is the loop needing *you*: an approval before a
deploy, a value only you have, a real-world action no agent can perform, a task the agent failed.

> **Off by default.** Declare no human call and the run is fully unattended, exactly as before this
> existed. Only a Rust driver can make the loop *wait*, and only where its author wrote the call;
> `agg.yaml` has no `hil` key. The worker's channel is opt-in per project and can never stop the loop.

**The worker asks and exits** — once you have turned that channel on by adding the paragraph from
[Letting the worker ask](CONFIG.md#letting-the-worker-ask) to your `AGG.md`. It never waits (a session
is a paid subprocess holding a git branch), so the question is recorded, you are paged, and the answer
arrives at the top of its next brief:

```bash
agg hil bool   "Firewall piercing for :443 requested. Done?"
agg hil choose "Which store?" --option postgres --option sqlite
agg hil input  "Which instance is prod?"
```

**A Rust driver may block**, because driver code runs *between* sessions. Only in Rust, at a call
site a human wrote — `agg.yaml` has no `hil` key, so the YAML path never blocks:

```rust
let ok = agg.hil_bool("Deploy v2.3 to prod?")?;      // blocks until answered
```

**Answering.** No workflow need be running — an ask outlives the run that raised it. Three channels,
one validator behind all of them, so a value off a closed option list is refused identically:

- the **CLI** below;
- the **web UI** — `agg serve` renders open asks above the scoreboard, with a button per option;
- `POST /api/answer` `{"id":…,"value":…}` for anything else that can speak HTTP.

```bash
agg status                    # lists open asks: id, question, how long it has waited
agg answer 4f2a 2             # a choice: the option name, or its 1-based number
agg answer 4f2a yes           # a yes/no ask
```

An answer off the recorded option list is refused with the options re-printed, and the ask stays
open. The first answer wins. An answer is written to `agg/private/asks.jsonl`, which a confined
worker cannot write — **that** is what makes "a human approved this" mean something. It is not put on
the steering bus: an answer is a durable fact, not a message to a running workflow, which is why it
is `agg answer` and not `agg send answer`.

There is **no timeout**: an idle process spends no tokens, and `agg stop` interrupts a wait. Two
rules keep that safe:

- **`work_time` excludes waiting.** Ceilings keep firing while blocked, so bound a run with humans in
  it on `work_time` (effort), not just `wall_time` (a deadline) — otherwise an overnight question
  aborts a healthy run. Both are **seconds**.
- **An answer unblocks the step; a judge still owns the verdict.** Never let a human's "done" satisfy
  a goal — re-run the check that looks at the world.

⛔ **Never ask for a secret's value.** The question and answer are written to disk. Ask for the
credential to be *placed* (env, keychain, `.env`) and confirm with `agg hil bool`.

→ [Asking a human](CONFIG.md#asking-a-human-hil) · worked driver: [`examples/hil.rs`](../examples/hil.rs)

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
  "evidence":  ["<file>", ...] // optional — files the verdict cites; agg keeps a copy
}
```

So a **binary** judge can print just `{"met": true}`; a numeric one might print, as **one** example:

```json
{"met": false, "value": 18, "max": 28, "target": 28, "rationale": "18/28 tests pass"}
```

**Evidence survives the judge.** A judge writes its working files (a SARIF report, a benchmark, a
diff) to `$AGG_JUDGE_SCRATCH`, which is per session and transient. Name a file in `evidence` and agg
copies it to `agg/private/evidence/<run_id>/<session>/<judge>/` after judging, records the copy on the
verdict's ledger row, and lists it in the run report — so a pull request can show *why* a gate
passed or failed. Only regular files inside the scratch or the project are kept; anything else is
refused and recorded. One run copies at most `judge.evidence_max_mb` (default 100)
([details](CONFIG.md#evidence--what-agg-keeps)).

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

**Numbers that may only get better: the ratchet.** An invariant keeps a `met` from flipping back; a
ratchet keeps a *number* from sliding back. Name the judge under `sequence.ratchet` with the direction
that is better and a tolerance for noise, and a session that makes it worse beyond that is rolled back
like any other regression:

```yaml
sequence:
  done_if: "tests_pass"
  ratchet:
    complexity: { direction: "down", tolerance: 0.5 }               # reference = last kept value
    coverage:   { direction: "up", tolerance: 1.0, baseline: 71.0 } # seed the first reference
  abort_if: "consecutive_rollbacks >= 3 OR oscillating OR over_budget"
```

`consecutive_rollbacks` (damage: the agent keeps making things worse) and `oscillating` (it fixes A,
breaks B, then fixes B and breaks A) are counted by the gate itself, in agg's memory — not read from a
file the worker could touch — so unlike `stalled` they are safe in `abort_if` on every isolation tier.
The full rules (references, drift warnings, which sessions count) are in
[`docs/CONFIG.md`](CONFIG.md#the-ratchet--numeric-no-regression).

**Measured judges without a script: swe-metrics.** Most quality gates measure software — complexity,
duplication, vulnerabilities, secrets. With [swe-metrics](https://github.com/ssenge/swe-metrics) on
`PATH`, declare such a judge in `judges:` instead of writing a script that runs a tool and parses it:

```yaml
judges:
  cc_p95:  { metric: { id: "complexity.cyclomatic", stat: "p95", tool: "lizard" }, max: 10 }
  vulns:   { metric: { id: "dependencies.vulnerabilities", labels: { severity: ["critical", "high"] } }, max: 0 }
  quality: { score: { definition: "swe-default", version: "2" }, min: 80 }
sequence:
  done_if: "tests_pass AND quality AND vulns"
  ratchet:
    cc_p95: { tolerance: 0.5 }   # `max:` already says lower is better
```

All such judges of a session share ONE `swe-metrics measure` (and its SARIF is kept as evidence). A
metric that was not measured — its analyzer skipped or failed — makes the judge **error** with the
analyzer's reason; it is never met. The score is computed against the report of the last kept state,
so a metric that got worse costs points. Selectors, `combine`, `scope: changed` and the score's
comparability rules are in [`docs/CONFIG.md`](CONFIG.md#judges-built-on-swe-metrics--metric-and-score).

On a `container` run swe-metrics runs where every judge does — in `judge.image`, on a read-only
project, with a fresh home per call — so it must be in that image, not on the host. The reference image
holds it with all its pinned tools; it is built from your swe-metrics checkout, and trivy reads the
host's vulnerability database read-only (seed it with `trivy image --download-db-only`):

```sh
docker/swe-metrics/build.sh ~/src/swe-metrics      # → agg-swe-metrics
```
```yaml
judge: { image: "agg-swe-metrics" }                # add your script judges' toolchain to it
```

**Several tools for one question: groups.** When three tools measure the same thing and disagree, name
them once and count over just them — *k of n* is a threshold in the condition, never on the group:

```yaml
sequence:
  groups:
    static_quality: ["sonar", "codescene", "lizard"]
  done_if: "tests_pass AND count_met(static_quality) >= 2"   # two of three; all of them: all_goals(static_quality)
  merge_requires_improvement: true                         # optional: merge only sessions that improved something
```

A group named in `done_if` is part of your DoD, and the gate judges it **net**: a session in which one
tool flips to unmet while another flips to met is not a regression; one that loses ground is rolled back.
With `merge_requires_improvement`, a session that regressed nothing but also moved no goal and no ratchet
(a pure reformat) is not merged — recorded `no_improvement`, which is not a rollback. Details in
[`docs/CONFIG.md`](CONFIG.md#judge-groups--k-of-n).

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

A long prompt does not have to live in `agg.yaml`: `prompt_file: "steps/reconsider.md"` (and
`role_prompt_file:`) reads it from a file under `agg/` instead — re-read at every session start, so an
edit applies from the next session, with each version's hash recorded in `agg/private/prompts.jsonl`
([details](CONFIG.md#prompts-from-files--prompt_file-role_prompt_file)).

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
                     sessions: Some(300), wall_time: Some(36000.0), work_time: None })
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

Protected paths work the same way on this path: `Step::template().write_deny(["tests/oracles/"])`
accumulates down the template chain like `readonly`, but nothing — not `.writable()` — subtracts from
it. A step that changed a protected path makes the next `gate()` return `RolledBack`, whatever the
judges and `on_regression` say.

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

**The worker's wiki is an OKF bundle.** `agg/state/wiki/` is the worker's durable knowledge base — one
concept per markdown file with a `type:` in its frontmatter, cross-linked, plus a reserved `index.md` and
`log.md` — in the [Open Knowledge Format v0.2](https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md),
so other tools (and an Obsidian vault opened on `agg/`) can read it as is. agg writes nothing into it;
the brief tells the worker the format. `agg wiki lint` checks it, and exits 1 if it is not conformant —
see [the wiki](CONFIG.md#the-wiki-okf-v02-and-agg-wiki-lint).

**`agg/private/` is carved out of the sandbox's writable set.** Under `isolation: sandbox` or
`container`, the worker may write the project dir and its own `agg/state/` but writes to `agg/private/`
are refused by the kernel — otherwise a confined worker could append forged `merged` rows to
`verdicts.jsonl`, make `stalled` report met, and end its own run under `abort_if: "stalled"`. **Reads
are untouched**: the worker still reads its brief and judges still read the ledger. Under the default
`isolation: none` the worker has your whole filesystem and no directory layout changes that — see
[isolation](CONFIG.md#isolation--blast-radius-jail-a-different-axis-from-session-isolation).

**Protected paths are checked, not just carved out.** `agg/private/` is protected by the kernel
alone, so under `none` it is a protocol boundary. The paths you list in `write_deny` get more: agg
snapshots them before every worker and compares after it, on every tier, and rolls back a session
that changed one — see [protected paths](CONFIG.md#protected-paths--write_deny-readonly-writable).

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

Global flags, valid on every subcommand: `--dir <path>` (project root, default `.`; `--project
<path>` is the same flag), `--config <file>` (default `<dir>/agg/agg.yaml`).

| Command | What it does | Flags |
|---|---|---|
| `agg init` | Scaffold **placeholder** config you must then edit — a blank-slate fallback. Prefer `/agg:new`, which fills it in from your project. Writes `agg/agg.yaml` + `agg/AGG.md` + a starter judge. | `--agent <a>` shape it for that agent · `--force` overwrite |
| `agg doctor` | Diagnose the setup: **every agent the sequence names** is on PATH, config parses, conditions valid, judges resolve, **the agents can do what the config asks**, skills installed | `--json` versioned report: every backend (installed, path, version, capability flags), the isolation tiers this host offers, every problem with its config path |
| `agg check` | Validate `agg/agg.yaml` + its judges exactly as `agg run` would at startup, listing **every** problem at once (each with a dotted config path). Runs no judge, no agent. Exit 1 on any problem | `--json` |
| `agg schema` | Print the JSON Schema of `agg.yaml`, generated from the config types | |
| `agg skills install` | Install the `agg-*` skills where your agent looks (`.claude/skills/` for Claude, `.agents/skills/` for Codex + Copilot) | `--agent <a>` (default: agg.yaml's `agent:`) · `--user` install under `$HOME` |
| `agg plan` | Run every run-set judge once and print the starting scoreboard (a dry run) | |
| `agg run` | Drive the loop until `done_if` is met (or a guard fires). Writes `agg/private/report.json` on every ending | `--max-sessions <n>` (0 = unlimited) · `--detach` / `-d` · `--report-json` logs to `agg/private/run.log`, prints only the report · `--base-branch <ref>` overrides `session_isolation.base_branch` · `--resume` continues the last interrupted run (same run id, next session, back to its last kept gate) |
| `agg judge <name>` | Resolve **one** judge by name and print its raw verdict — for authoring judges | |
| `agg status` | The loop's latest scoreboard, from its snapshot (cheap; re-runs no judges) | `--json` the versioned live status (`docs/schemas/status.v1.json`) |
| `agg history` | This project's run history, newest first, plus lifetime totals | `--json` (each run with its usage totals) |
| `agg dashboard` | Live TUI | `--once` one-shot text snapshot |
| `agg serve` | JSON API for the web UI: `/api/state`, `/api/history`, `/api/health`, `POST /api/send`, `POST /api/answer` | `--port <n>` (7878) · `--cors-origin <url>` · `--token <t>` |
| `agg spawn` | *(used by the worker, not to start the loop)* track a long child task so the reaper spares it and the next session polls it | `--name <n>` · `--reason <why>` · `-- <cmd…>` |
| `agg stop [reason]` | Graceful stop at the next session boundary (the one top-level steering alias) | |
| `agg send <cmd>` | Steering for a **running** workflow, applied at the next session boundary. Refused if none is running | `inject <text>` · `budget [total]` · `pause` · `resume` · `wake` (end a rate-limit wait now) · `stop [reason]` · `note <text>` · `answer <id> <value>` · `approve <id>` · `deny <id>` |
| `agg answer <id> <value>` | Answer an open human ask. Needs NO running workflow — an ask outlives the run that raised it. A value off a `choose`/`bool` ask's options is refused (exit 1) | `--json` `{schema_version, accepted, reason, id, answer}` |
| `agg hil <kind>` | *(used by the worker, not the operator)* ask a human. **Records and exits — it never waits** | `bool <q>` · `choose <q> --option … --option …` · `input <q>` |
| `agg wiki lint [path]` | Check the worker's wiki (`agg/state/wiki/`, or any `path`) against OKF v0.2: frontmatter with a non-empty `type` on every concept, a valid `index.md` and `log.md`. Read-only. Exit 1 on any violation | `--json` (`docs/schemas/wiki_lint.v1.json`) |

`agg run` exit codes, so automation can branch on the outcome: **0** `done_if` met · **1** hard
error · **3** a guard fired (`abort_if`) · **4** hit `--max-sessions` · **5** an operator stopped it
(`agg stop`, SIGTERM, SIGINT). The run report's `outcome` says the same thing and is authoritative —
see [Embedding agg](#embedding-agg).

> ⚠ **5 is new, and it changed.** An operator stop used to report **0**, sharing it with a met goal —
> so `if agg run; then ship; fi` shipped on `agg stop`. A stop is not success. A wrapper that wants
> the old behaviour writes `case $? in 0|5)`.
>
> A **Rust driver** needs `fn main() -> ExitCode { agg::driver::run(real_main) }` to get these codes
> at all: `-> Result<(), Fatal>` collapses every ending to 1. See [docs/RUST_API.md](RUST_API.md).

## Embedding agg: stable interfaces

A program that drives agg — a CI job, an orchestrator running many projects — should depend only on
these interfaces. A breaking change to a JSON document bumps its `schema_version`; a new field is not a
breaking change, so ignore unknown fields.

| Interface | Contract |
|---|---|
| **`agg/private/report.json`** | How the run ended: `outcome` (`done` · `aborted` · `stopped` · `error`), the reason, session counts, clock, every judge's last and baseline verdict (with the paths of its kept evidence) and ratchet, the stop detectors, the result commit, open asks, and usage — per session (`session_log`: worker and ruler apart, the model each agent reported, `input`/`output`/`cache_write`/`cache_read` tokens, `cost_usd` + `cost_source`) and in total. Schema: [`docs/schemas/report.v2.json`](schemas/report.v2.json) (v1 superseded). Field reference: [CONFIG.md](CONFIG.md#the-run-report-aggprivatereportjson). `agg run --report-json` prints it on stdout. |
| **`agg run` exit codes** | 0 done · 3 `abort_if` · 4 session cap · 5 stopped · 1 error. The report's `outcome` is authoritative. |
| **`agg status --json`** | Where a run stands now, from another process, without re-judging: `running`, `phase`, `current_step`, `session`, `time`, `sessions`, each judge's `last` verdict and `ratchet` reference, `detectors` (`stalled`, `oscillating`, `rollbacks`, `consecutive_rollbacks`, `ratchet_regressed`), `open_asks` with `options`, running `usage` totals, `waiting` (`{reason, agent, until}` during a rate-limit wait, else `null`). Field names match the report. Schema: [`docs/schemas/status.v1.json`](schemas/status.v1.json). Reference: [CONFIG.md](CONFIG.md#the-live-status-agg-status---json). |
| **`agg answer <id> <value> --json`** | `{schema_version, accepted, reason, id, answer}`; exit 0 accepted · 1 refused (unknown id, already answered, a value off the options). The answer reaches the worker's next brief, or the driver waiting on it. Schema: [`docs/schemas/answer.v1.json`](schemas/answer.v1.json). |
| `agg check --json` | `{schema_version, ok, config, problems: [{path, message}], warnings: [{path, message}]}`. `path` is dotted (`steps.worker.isolation`, `sequence.steps[1].until`; `""` = the whole file). Exit 0 valid · 1 any problem. Runs nothing. |
| `agg doctor --json` | `{schema_version, agg_version, ok, config: {path, found, valid, judges}, backends: [{name, bin, used, installed, path, version, self_sandboxes, capabilities: {reports_output_tokens, reports_cost_usd, supports_resume, supports_effort, detects_rate_limits, supports_one_shot}}], isolation: {none, sandbox, container: {available, mechanism, requested}}, container_engine, container_images: [{image, clis: {<bin>: <version line> \| null}, invisible, error}], network: {mode: open\|allow, allow, sandbox, container}, problems, warnings}`. Exit 0 ok · 1 otherwise. |
| `agg schema` | The JSON Schema (draft 2020-12) of `agg.yaml`, generated from the parser's own types; `$comment` names the agg version. |
| `agg wiki lint --json` | `{schema_version, ok, wiki, exists, files, concepts, problems: [{file, line, rule, message}], warnings}`. Exit 0 conformant · 1 a violation or a missing path. Schema: [`docs/schemas/wiki_lint.v1.json`](schemas/wiki_lint.v1.json). |
| The judge verdict JSON | One line on stdout: `{"met", "value", "max", "target", "rationale", "error"}` — see [Building judges](#building-judges). |

```bash
agg --project ./svc run --report-json --base-branch release > report.json
case "$(jq -r .outcome report.json)" in
  done) ship ;; aborted|stopped) escalate ;; *) page ;;
esac
```

**Headless runs.** `agg run` never prompts and never starts the TUI; it needs no TTY. With
`--report-json`, everything agg and its children print goes to `agg/private/run.log`, and stdout
carries only the report. SIGTERM is treated like `agg stop`: the worker is killed, the session ends,
the report says `stopped`, and the exit code is 5. `agg stop --project <dir>` stops a run from another
process. Runs in different project directories share nothing — pid file, scratch directories and the
report are all per project.

Poll `agg status --json` to watch a run and read `report.json` once `agg run` has exited:

```bash
agg --project ./svc status --json | jq '{running, session, step: .current_step.name, waiting, asks: [.open_asks[].id]}'
agg --project ./svc answer "$id" sqlite --json | jq .accepted
```

**Not** interfaces — internals that change without notice: `state.json`, the rows of
`verdicts.jsonl` and `calls.jsonl`, `project.json`, `gate.json`, the bus files, everything else under
`agg/private/` and `agg/state/`, and the human-readable text of every command. A report that is
**missing** (SIGKILL, OOM, power loss) must be read as `outcome: error`.

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
