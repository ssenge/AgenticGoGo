#!/bin/sh
echo "$RLT_STEP $1" >> .git/phase-hooks.log
echo "{\"met\": true, \"rationale\": \"phase hook $1\"}"
