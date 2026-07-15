#!/usr/bin/env bash
# Built-in judge: run $AGG_CMD; met = exit 0. Binary goal.
# Usage: a template — copy into your agg/judges/<name>.sh with AGG_CMD set (e.g. AGG_CMD='make build'),
# then name <name> in a condition. Library judges resolve by NAME and take no config parameters.
eval "${AGG_CMD:?set AGG_CMD}" >/tmp/agg_cmd.out 2>&1
code=$?
met=$([ "$code" -eq 0 ] && echo true || echo false)
tail=$(tail -1 /tmp/agg_cmd.out 2>/dev/null | tr '"' "'" | cut -c1-100)
printf '{"met":%s,"value":%s,"max":1,"target":1,"rationale":"exit %s: %s"}\n' \
  "$met" "$([ "$code" -eq 0 ] && echo 1 || echo 0)" "$code" "$tail"
