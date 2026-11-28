#!/bin/sh
# A script step: it runs in the project once the lanes are joined, and its last line is the verdict.
cat guide.md slides.md > docs.txt && git add docs.txt && git commit -qm "Collect the docs" \
  && printf '{"met": true, "rationale": "guide and slides collected"}\n' \
  || printf '{"met": false, "rationale": "could not collect the docs"}\n'
