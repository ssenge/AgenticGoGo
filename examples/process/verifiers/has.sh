#!/bin/sh
if [ -f "$1.enabled" ]; then m=true; else m=false; fi
echo "{\"met\": $m, \"rationale\": \"$1 is $m\"}"
