#!/usr/bin/env bash
# Standard library judge (INVARIANT): the build still passes — "never break a green build".
# Binary goal, meant for `sequence.invariants: [no_regression]`. Rust default; shadow for your lang.
out="$(cargo build 2>&1)"
code=$?
met=$([ "$code" -eq 0 ] && echo true || echo false)
tail=$(printf '%s' "$out" | grep -E '^error' | head -1 | tr '"' "'" | cut -c1-100)
printf '{"met":%s,"target":1,"rationale":"build %s%s"}\n' \
  "$met" "$([ "$code" -eq 0 ] && echo green || echo RED)" "$([ -n "$tail" ] && echo ": $tail")"
