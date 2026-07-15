#!/usr/bin/env bash
# Standard library judge: the working tree is CLEAN (no uncommitted tracked changes). Binary goal.
# agg's own runtime state is excluded so it never trips this.
n=$(git status --porcelain --untracked-files=no -- . ':(exclude)agg/state/**' 2>/dev/null | wc -l | tr -d ' ')
met=$([ "$n" -eq 0 ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":0,"rationale":"%s uncommitted tracked file(s)"}\n' \
  "$met" "$n" "$n" "$n"
