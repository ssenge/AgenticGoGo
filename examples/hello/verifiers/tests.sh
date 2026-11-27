#!/bin/sh
# A script verifier: it runs the program and prints one JSON line, the verdict. Relentless runs it, never the agent.
out=$(python3 add.py 2>&1)
if [ "$out" = "3" ]; then
  printf '{"met": true, "rationale": "add.py prints 3"}\n'
else
  printf '{"met": false, "rationale": "add.py printed: %s (expected 3)"}\n' "$out"
fi
