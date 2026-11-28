#!/bin/sh
# A script verifier: the guide is there. It runs in the lane's worktree.
if [ -s guide.md ]; then
  printf '{"met": true, "rationale": "guide.md exists"}\n'
else
  printf '{"met": false, "rationale": "guide.md is missing"}\n'
fi
