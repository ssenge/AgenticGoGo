#!/usr/bin/env bash
# MEASURABLE PARTIAL PROGRESS: how many NON-TRIVIAL supporting lemmas does Lean actually verify?
# Only counts if the project builds (so unbuildable lemmas don't inflate the number), excludes
# any file carrying a `sorry`, and — so the metric can't be gamed by padding with junk like
# `theorem tN : True := trivial` (the seed lemma is exactly that) — excludes declarations whose
# statement is the trivial `: True`. value = #verified non-trivial lemmas, target = 20.
TARGET=20
out="$(lake build 2>&1)"; rc=$?
if [ $rc -ne 0 ]; then
  echo "{\"met\":false,\"value\":0,\"max\":$TARGET,\"target\":$TARGET,\"rationale\":\"project does not build yet — 0 verified lemmas\"}"
  exit 0
fi
# count `theorem`/`lemma` declarations in sorry-free files, skipping trivial `: True` statements.
n=0
for f in $(find proof -name '*.lean' 2>/dev/null); do
  grep -qE '\bsorry\b' "$f" && continue
  # a lemma line that is NOT a `... : True ...` trivial-proof padding declaration
  c=$(grep -E '^[[:space:]]*(theorem|lemma)\b' "$f" | grep -vcE ':[[:space:]]*True\b')
  n=$((n + c))
done
met=$([ "$n" -ge "$TARGET" ] && echo true || echo false)
echo "{\"met\":$met,\"value\":$n,\"max\":$TARGET,\"target\":$TARGET,\"rationale\":\"$n non-trivial Lean-verified lemma(s) (sorry-free, project builds)\"}"
