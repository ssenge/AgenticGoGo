#!/bin/sh
# A check of a task force's repair in place (`task_force.checks`): rejects the first repair when the project says so
# (.git/rlt-fix-reject, which it removes), and logs what it was told.
echo "$RLT_STEP $RLT_FIX_BASE" >> .git/fix-check.log
if [ -f .git/rlt-fix-reject ]; then
  rm .git/rlt-fix-reject
  echo "{\"met\": false, \"rationale\": \"the repair broke a gate in $RLT_STEP\"}"
else
  echo "{\"met\": true, \"rationale\": \"the repair holds\"}"
fi
