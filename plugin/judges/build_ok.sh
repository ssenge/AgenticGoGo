#!/usr/bin/env bash
# Standard library judge: the project BUILDS. Binary goal — met on a clean build.
# Rust default (`cargo build`); shadow it in agg/judges/build_ok.sh for another toolchain.
# Takes no parameters (library judges are parameterless — §5.1).
out="$(cargo build 2>&1)"
code=$?
met=$([ "$code" -eq 0 ] && echo true || echo false)
tail=$(printf '%s' "$out" | grep -E '^error' | head -1 | tr '"' "'" | cut -c1-100)
printf '{"met":%s,"target":1,"rationale":"cargo build exit %s%s"}\n' \
  "$met" "$code" "$([ -n "$tail" ] && echo ": $tail")"
