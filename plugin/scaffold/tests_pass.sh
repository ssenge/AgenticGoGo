#!/usr/bin/env bash
# Starter verifier (rlt/verifiers/tests_pass.sh) — resolved by the NAME `tests_pass` in done_if.
# Prints a verdict JSON to stdout. REPLACE the body with your real check.
# Env rlt sets: RLT_SESSION, RLT_STEP, RLT_VERIFIER, RLT_PROJECT_DIR.
N="$(cat .passing 2>/dev/null || echo 0)"
TARGET=3
met=$([ "$N" -ge "$TARGET" ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s/%s tests pass (starter stub — replace me)"}\n' \
  "$met" "$N" "$TARGET" "$TARGET" "$N" "$TARGET"
