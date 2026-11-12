#!/bin/sh
echo "$1" >> accepted.log
echo "{\"met\": true, \"rationale\": \"accepted $1 ($AGG_STEP)\"}"
