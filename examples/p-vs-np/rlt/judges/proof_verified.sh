#!/usr/bin/env bash
# The prize judge: Lean's kernel accepts the FULL proof with no gaps AND no smuggled assumptions.
#
# `lake build` + a no-`sorry` grep is necessary but NOT sufficient: a worker can make both pass
# while cheating, by adding `axiom pnp : P ≠ NP` (or reaching the goal via `sorryAx` /
# `native_decide` / `Lean.ofReduceBool`). So we ALSO run the soundness invariant (no_sorry.sh),
# which forbids gaps, hand-declared axioms, and unsound decision procedures. Only when the
# project builds AND that soundness check passes do we report met:true.
#
# For a proof with a single named theorem, the gold standard is stricter still —
#   lake env lean --run -c '#print axioms <mainThm>'
# asserting the axiom set ⊆ {propext, Classical.choice, Quot.sound}. This demo has no single
# main theorem (it is honest partial progress), so the axiom/escape-hatch grep in no_sorry.sh is
# the enforced check. It is not "uncheatable" in the absolute sense — it is the strongest check
# available without a fixed target declaration.
here="$(cd "$(dirname "$0")" && pwd)"
out="$(lake build 2>&1)"; rc=$?
sound="$("$here/no_sorry.sh")"
sound_met="$(printf '%s' "$sound" | grep -o '"met":[a-z]*' | head -1 | cut -d: -f2)"

if [ $rc -eq 0 ] && [ "$sound_met" = "true" ]; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"Lean built the full proof and the soundness check passed — no gap, no smuggled axiom"}'
else
  if [ "$sound_met" != "true" ]; then
    reason="$(printf '%s' "$sound" | grep -o '"rationale":"[^"]*"' | cut -d: -f2- | tr -d '"')"
    echo "{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"soundness check failed: $reason\"}"
  else
    tail="$(printf '%s' "$out" | tail -1 | tr -d '"')"
    echo "{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"Lean does not accept the proof yet (rc=$rc): $tail\"}"
  fi
fi
