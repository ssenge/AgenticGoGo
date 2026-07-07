#!/usr/bin/env bash
# Built-in judge: count matches of $AGG_PATTERN; met when count <= $AGG_TARGET (default 0).
# Good for "no TODOs left", "no `unwrap()` in src", etc. Cardinal (lower is better → invert).
# Usage: cmd: "AGG_PATTERN='TODO' AGG_PATH='src' AGG_TARGET=0 ${CLAUDE_PLUGIN_ROOT}/judges/grep_count.sh"
#
# AGG_PATTERN/AGG_PATH come from goals.yaml, which is TRUSTED-AUTHOR config (the worker cannot
# edit the config that governs it). Even so we harden two footguns so a `-`-leading or
# quote-containing pattern behaves predictably rather than corrupting the verdict:
#   • pass the pattern after `-e … --` so it is never parsed as a grep option;
#   • JSON-escape the pattern before echoing it into the rationale, so a `"` or `\` can't
#     produce invalid JSON (which agg would read as a spurious judge failure).
set -u
pat="${AGG_PATTERN:?set AGG_PATTERN}"; path="${AGG_PATH:-.}"; target="${AGG_TARGET:-0}"
n=$(grep -rIn -e "$pat" -- "$path" 2>/dev/null | wc -l | tr -d ' ')
met=$([ "$n" -le "$target" ] && echo true || echo false)
# minimal JSON string-escape for the rationale (backslash and double-quote).
esc_pat=${pat//\\/\\\\}; esc_pat=${esc_pat//\"/\\\"}
esc_path=${path//\\/\\\\}; esc_path=${esc_path//\"/\\\"}
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s matches of /%s/ in %s (target <= %s)"}\n' \
  "$met" "$n" "$((n>target?n:target))" "$target" "$n" "$esc_pat" "$esc_path" "$target"
