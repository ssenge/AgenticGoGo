# `agg` configuration reference

Every key in `agg.yaml` and `goals.yaml`. The README lists only the knobs most runs touch.


**Don't re-check a finished goal (`recheck:`)** — by default `VERIFY` runs every goal's judge each
cycle. For a goal whose status can't change once achieved (a written report, a completed study)
that wastes work — especially with an LLM judge. Set a recheck policy in `goals.yaml`:

```yaml
- id: report_written
  recheck: once_met        # judge until first met, then LATCH — never re-judged (shown 🔒)
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

**What the agent can do — and constraining it.** `RUN` launches
`claude -p --dangerously-skip-permissions`: a headless `-p` agent can't answer permission prompts,
so it needs full tool access to make progress — which means **the agent runs with your user's full
host access**. The outer loop's rails (watchdog, budget/cost ceilings, git isolation, the rollback
gate) guard the *loop*; they do not sandbox the agent itself. For unattended overnight runs, prefer
running `agg` in a container/VM you're willing to hand to an autonomous agent.

To narrow what the agent may do, pass extra `claude` flags via `worker_args` in `agg.yaml`:

```yaml
worker_args: ["--allowedTools", "Edit,Bash", "--add-dir", "src"]   # or --disallowedTools, etc.
```

These are appended to every `RUN` (the judge and summarizer sessions run separately, *without*
`--dangerously-skip-permissions`, and load only your settings — never the agent-mutated repo's
`.claude/` config).

**No orphaned compute** *(macOS + Linux)*. The agent runs in its own process group; when a session
ends, the loop stops, or you `Ctrl-C`, `agg` sweeps the whole group and kills any straggler — even a
`nohup … &` or `--watch` child that escaped (POSIX process groups, no fragile env-reading).

> **Platform note.** `agg` is **unix-first** (macOS + Linux). The Windows binary builds and the
> **core outer loop runs** (INJECT → RUN → VERIFY → GATE, steering, dashboard), but two safety
> features are **not** implemented there: the **CPU-flat half of the watchdog** (a wedged agent is
> caught only by `over_iterations` / `agg stop`) and **process-group reaping**. `agg run` prints a
> one-line notice on Windows so this is never a surprise.

