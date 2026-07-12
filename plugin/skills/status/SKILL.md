---
name: agg-status
description: Show the current status of the AgenticGoGo loop for this project — the goal scoreboard, what's met/blocked, tokens spent, and the latest progress summary. Use when the user asks how the loop / agg run is doing.
disable-model-invocation: false
---

# /agg:status — report the AgenticGoGo loop status

Give the user a concise, accurate status of the running (or last) `agg run` loop.

## Step 1 — Read the live state

The loop writes a compact snapshot to `.agg/state.json` in the project. Read it:

```bash
cat .agg/state.json 2>/dev/null
```

If it's missing, the loop hasn't been started — tell the user to run `/agg:new` (to set up)
or `agg run` (if already configured), and stop here.

You can also run the dry-run scoreboard directly (re-evaluates judges right now):
```bash
agg plan
```
Use `agg plan` when the user wants a FRESH evaluation; use `.agg/state.json` when they want
the state of the currently-running loop without re-judging.

## Step 2 — Report

Summarize from the state JSON:

- **Headline**: `Goals N/M met` and the `stop_when` condition (are we close?).
- **Per goal**: id, type, current measure (`18/28`, `82%`, `yes`), lifecycle state
  (met ✔ / in_progress ◑ / regressed ⚠ / pending ·), and any `▲+N` delta. **Call out
  regressions loudly** — a goal that was met and broke is the most important signal.
- **Run health**: current `session`, `phase`, `idle_secs` (flag if ≥240 = possible stall),
  `tokens_spent` vs `budget_total`, `up_secs`.
- **Latest progress**: the `summary_cumulative` (story so far) and `summary_windowed`
  (last cycle) lines, plus the live `now`/`think` activity.
- **If `finished: true`**: report the `finish_reason` (stopped/halted/done).

Keep it tight — a scoreboard the user can read at a glance, not a wall of JSON. For the live
view, suggest `agg dashboard` in a second terminal.

## Step 3 — If asked "what should I do?"

- A regressed invariant → the loop likely halted; investigate that goal's judge rationale.
- Stalled (high idle, no token growth) → the watchdog should auto-recover; if not, the
  worker may be wedged — suggest checking `agg.log` / restarting `agg run`.
- Close to done (e.g. `met_fraction` high) → let it run.
- Out of budget (`over_budget`) → raise `budget.total` in `agg.yaml` and restart, or accept
  the partial result.
