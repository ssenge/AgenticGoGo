#!/bin/sh
n=$(wc -l < work.log 2>/dev/null | tr -d " "); n=${n:-0}
if [ "$n" -ge 1 ]; then m=true; else m=false; fi
echo "{\"met\": $m, \"rationale\": \"$n sessions\"}"
