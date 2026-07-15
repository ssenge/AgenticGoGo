#!/usr/bin/env bash
# Built-in judge: cargo test → verdict JSON. Counts passed/total from the summary.
# met when all tests pass. Parameterless: name `cargo_test` in a done_if/abort_if condition and it resolves by NAME.
out="$(cargo test 2>&1)"
# parse "test result: ok. N passed; M failed"
passed=$(printf '%s' "$out" | grep -oE '[0-9]+ passed' | awk '{s+=$1} END{print s+0}')
failed=$(printf '%s' "$out" | grep -oE '[0-9]+ failed' | awk '{s+=$1} END{print s+0}')
total=$((passed + failed))
met=$([ "$failed" -eq 0 ] && [ "$total" -gt 0 ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s/%s tests pass (%s failed)"}\n' \
  "$met" "$passed" "$total" "$total" "$passed" "$total" "$failed"
