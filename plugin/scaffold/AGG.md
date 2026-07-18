<!-- AGG.md — the standing instructions for this project (the CLAUDE.md-analog for the agg loop).
     COMMITTED; you (the human) own it; rare edits. agg points every worker session here for
     orientation. The moving "what to do next" lives in agg/state/STATE.md; durable knowledge and
     multi-session PLANS live in agg/state/wiki/. A vague file here = a loop that spins. -->

# Project
One line: what this project is and does.

# Goal
Make all the project's tests pass. (The real gate is your judges / `done_if`.)

# Architecture — where things live
Fill this in so a fresh worker orients fast: key modules/entry points, and the exact build/test
commands to run.

# Rules
- You are AUTONOMOUS. There is NO human to answer questions — never pause to ask.
- Real, correct work only — no stubs. Keep changes focused.
- Durable knowledge lives in `agg/state/wiki/` as an OKF (Open Knowledge Format) wiki: one concept
  per markdown file (HYPHENATED, space-free filenames so links resolve everywhere), a required `type:`
  frontmatter, CROSS-LINKED with standard `[label](page.md)` markdown links so it forms a graph. agg's
  per-session brief ships the exact format + a template. Keep any multi-session PLAN there (STATE.md is
  rewritten each session, so a plan parked there is lost; the wiki persists and survives rollbacks) and
  record dead-ends + decisions. View it in Obsidian by opening the `agg/` folder as a vault.
