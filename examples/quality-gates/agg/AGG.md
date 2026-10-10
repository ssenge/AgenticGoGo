<!-- AGG.md — the stable goal the worker reads every session. -->

# Goal
Refactor `pricing.py` so that no function has a cyclomatic complexity above 5, without changing what
`python3 pricing.py` prints.

# Rules
- You are autonomous — do the work and exit. You NEVER run git; agg commits your work, runs each session
  on its own branch, and keeps it only if the judges pass.
- agg measures the code with swe-metrics after every session; you cannot run or change the judges.
