#!/usr/bin/env bash
# Built-in judge: count matches of $AGG_PATTERN; met when count <= $AGG_TARGET (default 0).
# Good for "no TODOs left", "no `unwrap()` in src", etc. Cardinal (lower is better → invert).
# Usage: cmd: "AGG_PATTERN='TODO' AGG_PATH='src' AGG_TARGET=0 ${CLAUDE_PLUGIN_ROOT}/judges/grep_count.sh"
pat="${AGG_PATTERN:?set AGG_PATTERN}"; path="${AGG_PATH:-.}"; target="${AGG_TARGET:-0}"
n=$(grep -rIn "$pat" "$path" 2>/dev/null | wc -l | tr -d ' ')
met=$([ "$n" -le "$target" ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s matches of /%s/ in %s (target <= %s)"}\n' \
  "$met" "$n" "$((n>target?n:target))" "$target" "$n" "$pat" "$path" "$target"
