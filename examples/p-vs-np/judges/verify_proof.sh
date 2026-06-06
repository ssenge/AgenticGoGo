#!/usr/bin/env bash
# The real, uncheatable check: Lean compiles the proof AND there is no `sorry`. Lean's kernel
# rejects any proof with a gap, so "met:true" here genuinely means a complete formal proof.
out="$(lake build 2>&1)"; rc=$?
if [ $rc -eq 0 ] && ! grep -rqE '\bsorry\b' proof/ 2>/dev/null; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"Lean verified the full proof — no sorry"}'
else
  tail="$(printf '%s' "$out" | tail -1 | tr -d '"')"
  echo "{\"met\":false,\"value\":0,\"max\":1,\"target\":1,\"rationale\":\"Lean does not accept the proof yet (rc=$rc): $tail\"}"
fi
