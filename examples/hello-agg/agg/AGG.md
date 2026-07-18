<!-- AGG.md — the STABLE scope/goal for this project. You write it once; the worker reads it
     for orientation every session. Rare, human-owned edits. The forward "what to do next" advice
     lives in agg/state/STATE.md, which is created at runtime (gitignored), not shipped here. -->

# Goal
Fix `add.py` so that running `python3 add.py` prints exactly: 2

# Rules
- You are autonomous — do the work, COMMIT it (`git add -A && git commit`), and exit. agg runs each
  session on an isolated branch and keeps it only if committed and the judges pass; you never merge or push.
- Do one real chunk of work per session. No stubs, no placeholders.
