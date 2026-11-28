#!/bin/sh
# A script verifier: the unit's file is there. It runs in the lane's worktree; RLT_ITEM names the unit.
if [ -s "out/$RLT_ITEM.txt" ]; then
  printf '{"met": true, "rationale": "out/%s.txt exists"}\n' "$RLT_ITEM"
else
  printf '{"met": false, "rationale": "out/%s.txt is missing"}\n' "$RLT_ITEM"
fi
