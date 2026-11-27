<!-- RLT.md — the standing instructions for this project (the CLAUDE.md-analog for the rlt loop).
     COMMITTED; you (the human) own it; rare edits. rlt points every worker session here for
     orientation. The moving "what to do next" lives in rlt/state/STATE.md; durable knowledge and
     multi-session PLANS live in rlt/state/wiki/. A vague file here = a loop that spins. -->

# Project
One line: what this project is and does.

# Goal
Make all the project's tests pass. (The real gate is your verifiers / `done_if`.)

# Architecture — where things live
Fill this in so a fresh worker orients fast: key modules/entry points, and the exact build/test
commands to run.

# Rules
- You are AUTONOMOUS. There is NO human to answer questions — never pause to ask.
  <!-- OPT-IN: if this project WANTS the worker to be able to reach a human, add the paragraph from
       docs/CONFIG.md#letting-the-worker-ask here. Leave it out and the run stays fully unattended,
       which is the default and the point. -->
- Real, correct work only — no stubs. Keep changes focused.
- `rlt/state/` is YOURS (STATE.md, wiki/). `rlt/private/` is rlt's — read it if you like, never
  write it; under `isolation: sandbox` the attempt just fails.
- Durable knowledge lives in `rlt/state/wiki/` as an OKF (Open Knowledge Format) wiki: one concept
  per markdown file (HYPHENATED, space-free filenames so links resolve everywhere), a required `type:`
  frontmatter, CROSS-LINKED with standard `[label](page.md)` markdown links so it forms a graph. rlt's
  per-session brief ships the exact format + a template. Keep any multi-session PLAN there (STATE.md is
  rewritten each session, so a plan parked there is lost; the wiki persists and survives rollbacks) and
  record dead-ends + decisions. View it in Obsidian by opening the `rlt/` folder as a vault.
