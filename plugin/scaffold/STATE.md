<!-- STATE.md — your predecessor's forward advice: crisp "what to do next". agg regenerates the
     per-session brief (agg/private/INSTRUCTIONS.md) from this + AGG.md + memory, and points the
     worker at it. You (the agent) rewrite THIS file each session before you exit. Keep it SHORT —
     it is read in full. Gitignored, so it survives a session rollback. -->

# Where things stand
First session — nothing done yet.

# Next step
1. Orient: read agg/AGG.md, then run the project's tests/checks to see what's failing.
2. Implement or fix ONE thing that moves a goal forward. Real, correct work — no stubs.
3. Verify your change (re-run the relevant test/check).
4. Rewrite THIS file with the new state + the exact next task before you exit.
