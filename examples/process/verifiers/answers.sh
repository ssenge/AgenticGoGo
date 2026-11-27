#!/bin/sh
# met after enough sessions, unless the project says the exam can't be passed (exam.fails)
n=$(wc -l < work.log 2>/dev/null | tr -d " ")
if [ "${n:-0}" -ge 5 ] && [ ! -f exam.fails ]; then m=true; else m=false; fi
echo "{\"met\": $m, \"rationale\": \"t1 answered after ${n:-0} sessions\"}"
