#!/bin/sh
n=$(wc -l < work.log 2>/dev/null | tr -d " ")
if [ "${n:-0}" -ge 4 ]; then m=true; else m=false; fi
echo "{\"met\": $m, \"rationale\": \"${n:-0} sessions\"}"
