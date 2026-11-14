#!/usr/bin/env bash
# Fails when tracked files or a commit message contain a banned word. The words are never in this repo:
# they come from the file $DENY_LIST (one word per line) or from $DENY_WORDS (newline-separated, CI secret).
# Usage: deny-check.sh --tree | deny-check.sh --message <file>
set -euo pipefail
words=$(mktemp); trap 'rm -f "$words"' EXIT
if [ -n "${DENY_LIST:-}" ] && [ -f "$DENY_LIST" ]; then cat "$DENY_LIST" > "$words"
elif [ -n "${DENY_WORDS:-}" ]; then printf '%s\n' "$DENY_WORDS" > "$words"
else echo "deny-check: no DENY_LIST or DENY_WORDS set, nothing checked" >&2; exit 0; fi
sed -i.bak '/^[[:space:]]*$/d' "$words" && rm -f "$words.bak"
case "${1:-}" in
  --tree) hits=$(git grep -I -i -n -w -F -f "$words" -- . ':!scripts/deny-check.sh' || true) ;;
  --message) hits=$(grep -i -n -w -F -f "$words" "${2:?a message file}" || true) ;;
  *) echo "usage: deny-check.sh --tree | --message <file>" >&2; exit 2 ;;
esac
if [ -n "$hits" ]; then echo "deny-check: banned word found:" >&2; echo "$hits" | cut -c1-160 >&2; exit 1; fi
