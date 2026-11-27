#!/bin/sh
a=$(wc -l < accepted.log 2>/dev/null | tr -d " ")
if [ "${a:-0}" -ge 1 ]; then m=true; else m=false; fi
echo "{\"met\": $m, \"rationale\": \"${a:-0} units accepted before the last\"}"
