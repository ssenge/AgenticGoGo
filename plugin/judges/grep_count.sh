#!/usr/bin/env bash
# Built-in judge: count matches of $RLT_PATTERN; met when count <= $RLT_TARGET (default 0).
# Good for "no TODOs left", "no `unwrap()` in src", etc. Cardinal (lower is better → invert).
# Usage: a template — copy into your rlt/judges/<name>.sh, set RLT_PATTERN/RLT_PATH/RLT_TARGET, and
# name <name> in a condition. Library judges resolve by NAME and take no config parameters.
#
# RLT_PATTERN/RLT_PATH are baked into the judge file (TRUSTED-AUTHOR — the worker cannot edit the
# judge that governs it). Even so we harden two footguns so a `-`-leading or
# quote-containing pattern behaves predictably rather than corrupting the verdict:
#   • pass the pattern after `-e … --` so it is never parsed as a grep option;
#   • JSON-escape the pattern before echoing it into the rationale, so a `"` or `\` can't
#     produce invalid JSON (which rlt would read as a spurious judge failure).
set -u
pat="${RLT_PATTERN:?set RLT_PATTERN}"; path="${RLT_PATH:-.}"; target="${RLT_TARGET:-0}"
n=$(grep -rIn -e "$pat" -- "$path" 2>/dev/null | wc -l | tr -d ' ')
met=$([ "$n" -le "$target" ] && echo true || echo false)
# minimal JSON string-escape for the rationale (backslash and double-quote).
esc_pat=${pat//\\/\\\\}; esc_pat=${esc_pat//\"/\\\"}
esc_path=${path//\\/\\\\}; esc_path=${esc_path//\"/\\\"}
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s matches of /%s/ in %s (target <= %s)"}\n' \
  "$met" "$n" "$((n>target?n:target))" "$target" "$n" "$esc_pat" "$esc_path" "$target"
