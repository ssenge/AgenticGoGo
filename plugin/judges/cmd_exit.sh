#!/usr/bin/env bash
# Built-in judge: run $RLT_CMD; met = exit 0. Binary goal.
# Usage: a template — copy into your rlt/judges/<name>.sh with RLT_CMD set (e.g. RLT_CMD='make build'),
# then name <name> in a condition. Library judges resolve by NAME and take no config parameters.
eval "${RLT_CMD:?set RLT_CMD}" >/tmp/rlt_cmd.out 2>&1
code=$?
met=$([ "$code" -eq 0 ] && echo true || echo false)
tail=$(tail -1 /tmp/rlt_cmd.out 2>/dev/null | tr '"' "'" | cut -c1-100)
printf '{"met":%s,"value":%s,"max":1,"target":1,"rationale":"exit %s: %s"}\n' \
  "$met" "$([ "$code" -eq 0 ] && echo 1 || echo 0)" "$code" "$tail"
