#!/usr/bin/env bash
# SOUNDNESS INVARIANT (the anti-cheating showpiece): a `sorry`/`admit` is Lean's "gap here"
# placeholder — a proof containing one is NOT a proof. If any appears, halt the loop.
# Match sorry/admit as actual Lean TACTICS, not the words in comments/strings. Strip Lean
# comments first: line comments (-- …) and block comments (/- … -/), then look for the tactic.
hit=""
for f in $(find proof -name '*.lean' 2>/dev/null); do
  code="$(perl -0777 -pe 's{/-.*?-/}{}gs; s{--[^\n]*}{}g' "$f" 2>/dev/null || sed 's/--.*//' "$f")"
  if printf '%s' "$code" | grep -qE '\bsorry\b|\badmit\b'; then hit="$f"; break; fi
done
if [ -n "$hit" ]; then
  echo "{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"gap found (sorry/admit tactic) in $hit — not a proof\"}"
else
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"no sorry/admit tactic anywhere in proof/"}'
fi
