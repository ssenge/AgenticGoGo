#!/usr/bin/env bash
# SOUNDNESS INVARIANT (the anti-cheating showpiece). A "proof" is only real if Lean's kernel
# accepts it with NO gaps and NO smuggled-in assumptions. Two ways to cheat, both caught here:
#
#   1. Gap placeholders — `sorry` / `admit` tactics. A proof containing one is not a proof.
#   2. Smuggled axioms — a hand-declared `axiom foo : P ≠ NP`, or reaching a result through an
#      unsound escape hatch (`sorryAx`, `native_decide` / `Lean.ofReduceBool`). These make
#      `lake build` + a no-`sorry` grep pass while the statement is ASSUMED, not proven —
#      exactly the loophole a "just make the judge green" worker will find.
#
# We strip Lean comments first (line `-- …` and block `/- … -/`) so matches are real tactics /
# declarations, not prose. Any hit halts the loop (this goal is an invariant in agg.yaml).
#
# NOTE ON COVERAGE: the truly rigorous check is `#print axioms <mainThm>` asserting the axiom
# set ⊆ {propext, Classical.choice, Quot.sound}. That needs a fixed target declaration name;
# this demo has no single "main theorem" (honest partial progress), so we forbid ALL
# user-authored `axiom` declarations and the known unsound escape hatches instead. If you adapt
# this to a proof with one named theorem, prefer the `#print axioms` form — see verify_proof.sh.

hit=""
why=""
for f in $(find proof -name '*.lean' 2>/dev/null); do
  code="$(perl -0777 -pe 's{/-.*?-/}{}gs; s{--[^\n]*}{}g' "$f" 2>/dev/null || sed 's/--.*//' "$f")"
  # 1. gap placeholders
  if printf '%s' "$code" | grep -qE '\bsorry\b|\bsorryAx\b|\badmit\b'; then
    hit="$f"; why="gap placeholder (sorry/admit)"; break
  fi
  # 2a. hand-declared axioms
  if printf '%s' "$code" | grep -qE '^[[:space:]]*axiom\b'; then
    hit="$f"; why="smuggled 'axiom' declaration — assumed, not proven"; break
  fi
  # 2b. known unsound escape hatches
  if printf '%s' "$code" | grep -qE '\bnative_decide\b|\bofReduceBool\b'; then
    hit="$f"; why="unsound decision procedure (native_decide/ofReduceBool)"; break
  fi
done

if [ -n "$hit" ]; then
  echo "{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"$why in $hit — not a sound proof\"}"
else
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"no gap, no smuggled axiom, no unsound decide anywhere in proof/"}'
fi
