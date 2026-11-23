#!/bin/sh
# A check of a task force's repair in place (`task_force.checks`): rejects the first repair when the project says so
# (.git/agg-fix-reject, which it removes), and logs what it was told.
echo "$AGG_STEP $AGG_FIX_BASE" >> .git/fix-check.log
if [ -f .git/agg-fix-reject ]; then
  rm .git/agg-fix-reject
  echo "{\"met\": false, \"rationale\": \"the repair broke a gate in $AGG_STEP\"}"
else
  echo "{\"met\": true, \"rationale\": \"the repair holds\"}"
fi
