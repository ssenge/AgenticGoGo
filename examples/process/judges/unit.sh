#!/bin/sh
# a unit needs a session of its own: the work log is longer than the accepted units
w=$(wc -l < work.log 2>/dev/null | tr -d " "); a=$(wc -l < accepted.log 2>/dev/null | tr -d " ")
if [ "${w:-0}" -gt "${a:-0}" ]; then m=true; else m=false; fi
echo "{\"met\": $m, \"rationale\": \"unit $1: ${w:-0} sessions, ${a:-0} accepted\"}"
