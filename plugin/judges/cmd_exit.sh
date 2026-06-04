#!/usr/bin/env bash
# Built-in judge: run $AGG_CMD; met = exit 0. Binary goal.
# Usage: cmd: "AGG_CMD='make build' ${CLAUDE_PLUGIN_ROOT}/judges/cmd_exit.sh"
eval "${AGG_CMD:?set AGG_CMD}" >/tmp/agg_cmd.out 2>&1
code=$?
met=$([ "$code" -eq 0 ] && echo true || echo false)
tail=$(tail -1 /tmp/agg_cmd.out 2>/dev/null | tr '"' "'" | cut -c1-100)
printf '{"met":%s,"value":%s,"max":1,"target":1,"rationale":"exit %s: %s"}\n' \
  "$met" "$([ "$code" -eq 0 ] && echo 1 || echo 0)" "$code" "$tail"
