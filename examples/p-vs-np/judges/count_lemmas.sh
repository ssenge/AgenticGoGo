#!/usr/bin/env bash
# MEASURABLE PARTIAL PROGRESS: how many supporting lemmas does Lean actually verify?
# Only counts if the project builds (so unbuildable lemmas don't inflate the number) and
# excludes any file carrying a `sorry`. value = #verified lemmas, target = 20.
TARGET=20
out="$(lake build 2>&1)"; rc=$?
if [ $rc -ne 0 ]; then
  echo "{\"met\":false,\"value\":0,\"max\":$TARGET,\"target\":$TARGET,\"rationale\":\"project does not build yet — 0 verified lemmas\"}"
  exit 0
fi
# count `theorem`/`lemma` declarations in files that have NO sorry
n=0
for f in $(find proof -name '*.lean' 2>/dev/null); do
  grep -qE '\bsorry\b' "$f" && continue
  c=$(grep -cE '^[[:space:]]*(theorem|lemma)\b' "$f")
  n=$((n + c))
done
met=$([ "$n" -ge "$TARGET" ] && echo true || echo false)
echo "{\"met\":$met,\"value\":$n,\"max\":$TARGET,\"target\":$TARGET,\"rationale\":\"$n Lean-verified lemma(s) (sorry-free, project builds)\"}"
