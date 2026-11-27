#!/usr/bin/env bash
# Standard library verifier: the working tree is CLEAN (no uncommitted tracked changes). Binary goal.
# rlt's own runtime state is excluded so it never trips this — BOTH halves, worker-writable
# rlt/state/ and rlt-owned rlt/private/ (same pathspec pair src/git/mod.rs commits with).
n=$(git status --porcelain --untracked-files=no -- . ':(exclude)rlt/state/**' ':(exclude)rlt/private/**' 2>/dev/null | wc -l | tr -d ' ')
met=$([ "$n" -eq 0 ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":0,"rationale":"%s uncommitted tracked file(s)"}\n' \
  "$met" "$n" "$n" "$n"
