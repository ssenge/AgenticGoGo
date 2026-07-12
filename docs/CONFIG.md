# `agg` configuration reference

The keys most runs touch beyond the README's quick start.

**First, the one that changes everything: `agent:`.** `agg.yaml`'s `agent:` picks which coding agent
the loop drives — `claude` (default), `codex`, or `copilot`. They are **not interchangeable**, and a
config that asks for something your agent cannot do is **refused at startup**, never silently
ignored. The full matrix lives in the README under
[Choosing an agent](../README.md#choosing-an-agent); the three rules that bite most often:

| | rule |
|---|---|
| `model:` | **Codex: omit it.** Naming a model you aren't entitled to is a hard 400. **Copilot: `auto`.** |
| `effort:` | **Copilot cannot combine `effort:` with `model: auto`** (its default) — agg refuses the pair. |
| `cost:` / `over_cost` | **Claude only.** Codex reports no dollars; Copilot bills in AI Credits. Use `budget:` (tokens) — it works on all three. Copilot can also self-cap: `worker_args: ["--max-ai-credits", "50"]`. |


**Stopping: `stop_when` vs `halt_when`.** Both live in `goals.yaml`, both are **optional**, and both
use the same mini-language (goal ids joined with `and` / `or` / `not`, plus aggregates like
`met_fraction`, `any_regressed(invariants)`, and guards like `over_budget` / `over_cost` /
`over_iterations` / `wall_hours` — note `over_cost` needs an agent that reports dollars, so it is
**Claude-only**). The difference is only what happens when the expression is true:

- **`stop_when`** — the **success** condition. When true, the loop stops and exits **0**. Default:
  `all_goals` (stop once every goal is met). This is the one nearly every run sets; the README's
  examples use only this.
- **`halt_when`** — an optional **guard**. When true, the loop aborts as a **failure** and exits
  **3**, so CI/automation can tell a guardrail bail (budget blown, an invariant regressed) apart from
  a real win. Default: none. Typical value — works on **any** agent:
  `any_regressed(invariants) OR over_budget OR over_iterations OR wall_hours >= 4`
  On `agent: claude` you can add `OR over_cost`. **Do not add it on Codex or Copilot** — neither can
  report a dollar figure, so `agg run` refuses the config outright rather than leave you with a spend
  guard that could never fire.

You can't fold the guards into `stop_when` — putting `over_budget` there would report a budget blowout
as *success*. That exit-code distinction is the only thing `halt_when` adds; if you don't need it
(most interactive runs don't), just omit it.


**Don't re-check a finished goal (`recheck:`)** — by default `VERIFY` runs every goal's judge each
cycle. For a goal whose status can't change once achieved (a written report, a completed study)
that wastes work — especially with an LLM judge. Set a recheck policy in `goals.yaml`:

```yaml
- id: report_written
  recheck: once_met        # judge until first met, then LATCH — never re-judged (shown 🔒)
  # `model:` uses YOUR agent's model names (`haiku` is Claude's). Omit it to take the agent's
  # default judge model — the only portable choice, and required on Codex.
  judge: { kind: llm, model: haiku, rubric: "judges/report.md", inputs: ["REPORT.md"] }

- id: artifact_valid
  recheck: on_change       # re-judge only when a declared input changes (by content hash)
  recheck_inputs: ["build/out.json"]
  judge: { kind: script, cmd: "./judges/validate.sh" }
```

`always` (default) is required for invariants — their status can regress, so `agg` rejects
`once_met` on an `invariant: true` goal.

**Wire in your own tooling (generic hooks).** `agg` is tool-agnostic: it runs *your* shell commands
at lifecycle moments and prepends *your* text to the `INJECT`ed prompt. Use this for a code-graph
builder, a memory cache, a linter — whatever you use. Nothing is hardcoded.

```yaml
hooks:
  on_start:         ["mytool build ."]      # once at startup
  on_session_start: ["mytool refresh ."]    # before each RUN
  on_session_end:   ["mytool persist ."]    # after each VERIFY
  on_stop:          ["mytool export ."]     # once when the loop stops
  background:       ["mytool --watch ."]    # long-lived; reaped automatically on stop
prompt_includes:
  - "AGG_TOOLING.md"                        # your text, prepended to every agent prompt
```

A failing hook is logged, never fatal. `background` processes are spawned in the loop's reaping
domain, so a `--watch` can't leak (see below).

**What the agent can do — and constraining it.** `RUN` launches the worker with that agent's
auto-approve flag (`claude --dangerously-skip-permissions`, `codex
--dangerously-bypass-approvals-and-sandbox`, `copilot --allow-all-tools`): a headless agent can't
answer permission prompts, so it needs full tool access to make progress — which means **the agent
runs with your user's full host access**. The outer loop's rails (watchdog, budget/cost ceilings, git
isolation, the rollback gate) guard the *loop*; they do not sandbox the agent itself. For unattended
overnight runs, prefer running `agg` in a container/VM you're willing to hand to an autonomous agent.

To narrow what the agent may do, pass extra flags **for your agent** via `worker_args` in `agg.yaml`
— they are passed through verbatim, so the vocabulary is that agent's own:

```yaml
worker_args: ["--allowedTools", "Edit,Bash", "--add-dir", "src"]   # claude
worker_args: ["--max-ai-credits", "50"]                            # copilot — its own spend ceiling
```

These are appended to every `RUN`. The judge and summarizer run as separate, tools-off calls that
load only your own settings — never the agent-mutated repo's config.

**No orphaned compute** *(macOS + Linux)*. The agent runs in its own process group; when a session
ends, the loop stops, or you `Ctrl-C`, `agg` sweeps the whole group and kills any straggler — even a
`nohup … &` or `--watch` child that escaped (POSIX process groups, no fragile env-reading).

> **Platform note.** `agg` is **unix-first** (macOS + Linux). The Windows binary builds and the
> **core outer loop runs** (INJECT → RUN → VERIFY → GATE, steering, dashboard), but two safety
> features are **not** implemented there: the **CPU-flat half of the watchdog** (a wedged agent is
> caught only by `over_iterations` / `agg stop`) and **process-group reaping**. `agg run` prints a
> one-line notice on Windows so this is never a surprise.

