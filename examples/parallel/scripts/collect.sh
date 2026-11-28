#!/bin/sh
# A script step: it runs in the project once the lanes are joined, and its last line is the verdict.
cat out/*.txt > summary.txt && git add summary.txt && git commit -qm "Collect the units" \
  && printf '{"met": true, "rationale": "%s units collected"}\n' "$(ls out | wc -l | tr -d ' ')" \
  || printf '{"met": false, "rationale": "could not collect the units"}\n'
