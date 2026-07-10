<!-- AGG_RESUME.md — the prompt fed to EVERY fresh worker session.
     This is the single most important file: a vague prompt = a loop that spins.
     Make it specific to YOUR project. Keep the autonomous-loop structure below. -->

# Goal
<!-- One or two sentences: what should be true when this is done? -->
Make all the project's tests pass.

# This session — do ONE self-contained chunk of work
1. Orient: read any handoff/state file, then run the project's tests/checks to see what's failing.
2. Implement or fix ONE thing that moves a goal forward. Do real, correct work — no stubs.
3. Verify your change (re-run the relevant test/check).
4. If there's a HANDOFF file, update it with the new state + the exact next task; commit.

# Rules
- You are AUTONOMOUS. There is NO human to answer questions — never pause to ask.
- `claude -p` does not auto-compact; when context fills you just stop. So BEFORE that:
  finish the current chunk, write the handoff, commit, then exit. The loop relaunches you fresh.
- Commit as you go. Keep changes focused and correct.
- LONG TASKS (a sim/build/benchmark that runs longer than one turn): do NOT launch it with a
  bare `nohup … &` and then idle-wait — your session ends when your turn does, which kills or
  orphans the task, and the next session relaunches a duplicate. Instead run it via
  `agg spawn --name <id> --reason "<why>" -- <cmd>`. agg keeps it alive past your session,
  PROTECTS it from the straggler reaper, and tells the next session it's running (and why).
  Then EXIT. A later session is told about it and polls its log (`.agg/spawns/<id>.log`) —
  consuming the result when it finishes — instead of starting over. One spawn per task; check
  the BACKGROUND TASKS block at the top of your prompt before launching anything.

<!-- If your project uses a spec/plan tool (get-shit-done, a ROADMAP, etc.), paste the
     relevant execution steps HERE — skills are NOT invocable in headless `agg run`. -->
