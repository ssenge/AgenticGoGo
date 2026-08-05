#!/usr/bin/env bash
# Release an `agg.block(..)` FROM OUTSIDE — the operator's half of the one call that stops a loop.
#
# `agg.block(msg)` (src/driver/facade.rs) is the driver saying "I cannot proceed without a human".
# It pings, then WAITS on the operator bus, re-checking the ceilings every 5s, until a `resume`
# arrives (or a `stop`, or a ceiling ends the run). The worker cannot reach it and cannot answer it:
# the bus lives under `agg/private/`, which is carved out of every worker's writable set. The
# operator answers it, and this is how.
#
#   ./scripts/agg_unblock.sh <project-dir>                     # send ONE resume, now
#   ./scripts/agg_unblock.sh <project-dir> --stop "reason"      # end the run instead
#   ./scripts/agg_unblock.sh <project-dir> --watch <driver.log> # unattended: resume EVERY block
#   ./scripts/agg_unblock.sh <project-dir> --watch <log> --answer "why it was ok to proceed"
#
# `--watch` is for an unattended run (an overnight sample, CI): it tails the driver's own output,
# and each time a new `[block]` announcement appears it queues one `resume`. It answers exactly as
# many blocks as it sees — never a pre-armed resume that a LATER block would swallow, which is the
# failure mode `block()` guards against by clearing `operator.resumed` on entry.
#
# ⚠ Auto-answering is a POLICY CHOICE, and the wrong one for a real release gate. It is right for a
# harness that is proving the loop loops; it is wrong when the question is "should this ship?".
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGG="${AGG_BIN:-$ROOT/target/debug/agg}"
DIR=""; WATCH=""; ANSWER=""; STOP=""; POLL="${AGG_UNBLOCK_POLL:-5}"; MAX_SECS="${AGG_UNBLOCK_MAX_SECS:-14400}"

while [ $# -gt 0 ]; do
  case "$1" in
    --watch)  WATCH="$2"; shift 2 ;;
    --answer) ANSWER="$2"; shift 2 ;;
    --stop)   STOP="${2:-operator stop}"; shift 2 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) DIR="$1"; shift ;;
  esac
done
[ -n "$DIR" ] && [ -d "$DIR" ] || { echo "usage: $0 <project-dir> [--watch <log>] [--answer <text>] [--stop <reason>]" >&2; exit 2; }
[ -x "$AGG" ] || { echo "no agg binary at $AGG (cargo build)" >&2; exit 1; }

# One command onto the project's bus. `agg send` writes it atomically (tmp + rename) and the loop
# drains it on its next 5s pass; it queues fine even before a run exists.
send() { ( cd "$DIR" && "$AGG" send "$@" ) >/dev/null 2>&1; }

if [ -n "$STOP" ]; then
  send stop "$STOP"                       # `agg send stop <reason>` — the reason is positional
  echo "queued: stop ($STOP)"
  exit 0
fi

if [ -z "$WATCH" ]; then
  [ -n "$ANSWER" ] && send note "$ANSWER"
  send resume
  echo "queued: resume → $DIR (the block releases within ~5s)"
  exit 0
fi

# ── watch mode ──────────────────────────────────────────────────────────────────────────────
# The driver prints `  [block] <msg>` when it starts waiting and `  [block] resumed by the
# operator.` when it stops. Announcements = the first minus the second, so the counter is right
# even if a human answers one by hand while this is running.
echo "unblock: watching $WATCH for [block] (poll ${POLL}s, giving up after ${MAX_SECS}s)"
answered=0; waited=0
while [ "$waited" -lt "$MAX_SECS" ]; do
  if [ -f "$WATCH" ]; then
    # `grep -c` PRINTS 0 and EXITS 1 on no match, so a `|| echo 0` fallback would yield "0\n0" and
    # break the arithmetic below. Take the count, default only if grep printed nothing at all.
    seen=$(grep -c '\[block\] ' "$WATCH" 2>/dev/null); seen=${seen:-0}
    resumed=$(grep -c '\[block\] resumed by the operator' "$WATCH" 2>/dev/null); resumed=${resumed:-0}
    pending=$(( seen - resumed ))
    while [ "$pending" -gt "$answered" ]; do
      answered=$((answered + 1))
      msg=$(grep '\[block\] ' "$WATCH" | grep -v 'resumed by the operator' | tail -1)
      echo "unblock: releasing block #$answered —${msg#*\[block\]}"
      [ -n "$ANSWER" ] && send note "$ANSWER"
      send resume
    done
    # run.pid is written at boot and cleared by the Drop guard, so "seen, then gone" is the end of
    # the run. It must be SEEN first: a watcher started before the driver booted would otherwise
    # read the not-yet-written pidfile as "already finished" and quit before the first block.
    if [ -f "$DIR/agg/private/run.pid" ]; then
      seen_pid=1
    elif [ "${seen_pid:-0}" = 1 ]; then
      echo "unblock: run finished (run.pid cleared) — answered $answered block(s)"
      exit 0
    elif [ "$waited" -ge "${AGG_UNBLOCK_BOOT_GRACE:-120}" ] && [ -s "$WATCH" ]; then
      echo "unblock: no run.pid ever appeared — nothing to answer" >&2
      exit 0
    fi
  fi
  sleep "$POLL"; waited=$((waited + POLL))
done
echo "unblock: gave up after ${MAX_SECS}s — answered $answered block(s)" >&2
exit 1
