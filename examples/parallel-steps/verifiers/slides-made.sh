#!/bin/sh
# A script verifier: the slides are there. It runs in the lane's worktree.
if [ -s slides.md ]; then
  printf '{"met": true, "rationale": "slides.md exists"}\n'
else
  printf '{"met": false, "rationale": "slides.md is missing"}\n'
fi
