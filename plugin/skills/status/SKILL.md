---
name: agg-status
description: Show the current status of the AgenticGoGo loop for this project — the judge scoreboard, what's met/blocked, tokens spent, and the latest progress summary. Use when the user asks how the loop / agg run is doing.
disable-model-invocation: false
---

# /agg:status — report the AgenticGoGo loop status

Give the user a concise, accurate status of the running (or last) `agg run` loop.

## Step 1 — Read the live state

The loop writes a compact snapshot to `agg/private/state.json` in the project. Read it:

```bash
cat agg/private/state.json 2>/dev/null
```

(`agg/private/` is agg's own runtime state — the half of the layout a confined worker cannot write.
The worker's own files live next door in `agg/state/`. Reading either is always fine.)

If it's missing, the loop hasn't been started — tell the user to run `/agg:new` (to set up)
or `agg run` (if already configured), and stop here.

You can also run the dry-run scoreboard directly (re-evaluates every judge right now):
```bash
agg plan
```
Use `agg plan` when the user wants a FRESH evaluation; use `agg/private/state.json` when they
want the state of the currently-running loop without re-judging.

## Step 2 — Report

Summarize from the state JSON:

- **⏳ OPEN ASKS FIRST, above everything else.** If `asks` is non-empty the loop is **waiting on a
  human and cannot proceed** — there is no timeout, so it waits indefinitely. This outranks the
  scoreboard: report the question, how long it has been waiting (`age_secs`), whether the `driver`
  or the `worker` asked, and the exact command to unblock it:
  ```bash
  agg send answer <id> "<value>"   # a choice takes an option name or its 1-based number
  agg send approve <id>            # yes/no sugar          agg send deny <id>
  ```
  A `worker`-origin ask does NOT block the loop (the worker asked and ended its session); a
  `driver`-origin one does. Say which. If an ask is hours old, lead with that — a forgotten ask is
  the failure mode this design deliberately accepts, and noticing it is your job.
- **Headline**: `N/M judges met` and the `done_if` condition (are we close?). `done_if` is the
  project's Definition of Done, composed from judge names.
- **Per judge**: name, current measure (`18/28`, `82%`, `yes`), lifecycle state
  (met ✔ / in_progress ◑ / regressed ⚠ / pending ·), and any `▲+N` delta. **Call out
  regressions loudly** — a judge that was met and broke is the most important signal (the gate
  rolls that session back).
- **Current step**: which step is running, and its **agent + model** (a mixed sequence runs
  different agents at different steps — say which one is live).
- **Run health**: current `session`, `phase`, `idle_secs` (flag if ≥240 = possible stall),
  `tokens_spent` vs the `sequence.limits.tokens` ceiling, `up_secs`. Note the **per-agent** token/cost
  breakdown if present — a mixed run's single total is otherwise uninterpretable.
- **Latest progress**: the `summary_cumulative` (story so far) and `summary_windowed`
  (last cycle) lines, plus the live `now`/`think` activity.
- **If `finished: true`**: report the `finish_reason` (stopped/aborted/done).

Keep it tight — a scoreboard the user can read at a glance, not a wall of JSON. For the live
view, suggest `agg dashboard` in a second terminal.

## Step 3 — If asked "what should I do?"

- A regressed invariant → the loop likely aborted; investigate that judge's rationale.
- Stalled (high idle, no token growth) → the watchdog should auto-recover; if not, the
  worker may be wedged — suggest checking `agg/private/run.log` / restarting `agg run`.
- Close to done (e.g. `met_fraction` high) → let it run.
- Out of budget (`over_budget`) → raise `sequence.limits.tokens` in `agg/agg.yaml` and restart,
  or accept the partial result.
