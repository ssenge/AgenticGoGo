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
- You are AUTONOMOUS: **never pause to wait for anyone.** Nothing is watching your session, so a
  question you stop on is a session that burns tokens doing nothing.
- If you hit something ONLY a human can resolve — a missing credential, a decision you are not
  allowed to make, a real-world action (provision an account, open a firewall, sign something) —
  ask through agg and **END YOUR SESSION IMMEDIATELY**:
  ```bash
  agg hil bool   "Firewall piercing for :443 requested. Done?"
  agg hil choose "Which store?" --option postgres --option sqlite
  agg hil input  "Which instance is prod?"
  ```
  These record the question and exit at once — they never wait, and there is no flag that makes
  them. A human is paged, and their answer arrives at the top of your NEXT session's brief. Do not
  guess, do not fabricate, and do not poll for the answer.
- ⛔ **Never ask for a secret's VALUE.** Your question and its answer are written to disk. Ask for
  the credential to be PLACED (in the environment, a keychain, a `.env`) and confirm with
  `agg hil bool` — an answer may name a secret, never contain one.
- Real, correct work only — no stubs. Keep changes focused.
- `agg/state/` is YOURS (STATE.md, wiki/). `agg/private/` is agg's — read it if you like, never
  write it; under `isolation: sandbox` the attempt just fails.
- Durable knowledge lives in `agg/state/wiki/` as an OKF (Open Knowledge Format) wiki: one concept
  per markdown file (HYPHENATED, space-free filenames so links resolve everywhere), a required `type:`
  frontmatter, CROSS-LINKED with standard `[label](page.md)` markdown links so it forms a graph. agg's
  per-session brief ships the exact format + a template. Keep any multi-session PLAN there (STATE.md is
  rewritten each session, so a plan parked there is lost; the wiki persists and survives rollbacks) and
  record dead-ends + decisions. View it in Obsidian by opening the `agg/` folder as a vault.
