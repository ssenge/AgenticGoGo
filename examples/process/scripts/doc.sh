#!/bin/sh
mkdir -p checkpoints
echo "# $1 of $AGG_STEP_LABEL" > "checkpoints/$1.md"
echo "{\"met\": true, \"rationale\": \"document $1 written\"}"
