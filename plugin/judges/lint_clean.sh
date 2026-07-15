#!/usr/bin/env bash
# Standard library judge: the linter is CLEAN. Binary goal.
# Rust default (`cargo clippy -- -D warnings`); shadow in agg/judges/lint_clean.sh for another lang.
out="$(cargo clippy --all-targets -- -D warnings 2>&1)"
code=$?
n=$(printf '%s' "$out" | grep -cE '^(warning|error)')
met=$([ "$code" -eq 0 ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":0,"rationale":"clippy exit %s, %s diagnostic(s)"}\n' \
  "$met" "$n" "$n" "$code" "$n"
