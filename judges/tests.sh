#!/usr/bin/env bash
# Starter judge — prints a verdict JSON to stdout.
# This stub reads a count from a `.passing` file (default 0) and reports N/3.
# REPLACE the body with your real check, e.g.:
#   out="$(npm test 2>&1)"; passed=$(echo "$out" | grep -oE '[0-9]+ passing' | grep -oE '[0-9]+')
#   ...then emit the JSON below with your real numbers.
N="$(cat .passing 2>/dev/null || echo 0)"
TARGET=3
met=$([ "$N" -ge "$TARGET" ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s/%s tests pass (starter stub — replace me)"}\n' \
  "$met" "$N" "$TARGET" "$TARGET" "$N" "$TARGET"
