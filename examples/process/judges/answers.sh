#!/bin/sh
n=$(wc -l < work.log 2>/dev/null | tr -d " ")
if [ "${n:-0}" -ge 5 ]; then m=true; else m=false; fi
echo "{\"met\": $m, \"rationale\": \"t1 answered after ${n:-0} sessions\"}"
