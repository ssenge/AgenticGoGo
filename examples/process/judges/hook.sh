#!/bin/sh
echo "$RLT_STEP" >> .git/step-hooks.log
echo "{\"met\": true, \"rationale\": \"hook ran for $RLT_STEP\"}"
