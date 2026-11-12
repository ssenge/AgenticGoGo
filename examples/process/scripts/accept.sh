#!/bin/sh
# accepts the item; the checkpoint after the loop asks about the document this builds up (no `prepare` of its own)
echo "$1" >> accepted.log
mkdir -p checkpoints
echo "$1 since $2" >> checkpoints/build.md
echo "{\"met\": true, \"rationale\": \"accepted $1 ($AGG_STEP)\"}"
