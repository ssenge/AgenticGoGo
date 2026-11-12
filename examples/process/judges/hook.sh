#!/bin/sh
echo "$AGG_STEP" >> .git/step-hooks.log
echo "{\"met\": true, \"rationale\": \"hook ran for $AGG_STEP\"}"
