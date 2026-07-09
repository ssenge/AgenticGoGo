#!/usr/bin/env bash
# REAL end-to-end acceptance: `agg` driving the ACTUAL `claude` CLI against a real model.
#
# scripts/e2e.sh stubs the worker so it is fast, free and deterministic. This one does not.
# It spends real tokens, and it is the only test that exercises the worker integration for
# real: the live stream-json shapes, the real `session_id` and `--resume` continuity, the real
# `total_cost_usd` and `usage.output_tokens`, the real activity events, and a real agent
# actually satisfying an external judge.
#
# The `claude` on PATH is a PASSTHROUGH WRAPPER, not a stub: it records argv, records the
# phase agg had published when the worker started, and then `exec`s the real binary. Nothing
# about the model's behaviour is faked.
#
#   ./scripts/e2e_real.sh                          # ~1 min, a few cents on haiku
#   ./scripts/e2e_real.sh --model claude-sonnet-5
#   KEEP=1 ./scripts/e2e_real.sh                   # keep the workspace
#
# Exits 0 only if every check passed.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WS="${TMPDIR:-/tmp}/agg-e2e-real.$$"
AGG="$ROOT/target/debug/agg"
MODEL="claude-haiku-4-5-20251001"
[ "${1:-}" = "--model" ] && MODEL="$2"

PASS=0; FAIL=0; declare -a FAILED=()
sec()  { printf '\n\033[1m── %s\033[0m\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf '  \033[32m✔\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); FAILED+=("$1"); printf '  \033[31m✘ %s\033[0m\n' "$1"; [ -n "${2:-}" ] && printf '      %s\n' "$2"; return 0; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$3], got [$2]"; }
has()  { grep -qF -- "$3" "$2" 2>/dev/null && ok "$1" || bad "$1" "'$3' not found in $2"; }
hasnt(){ grep -qF -- "$3" "$2" 2>/dev/null && bad "$1" "'$3' unexpectedly present" || ok "$1"; }
exists(){ [ -e "$2" ] && ok "$1" || bad "$1" "missing: $2"; }
snap() { python3 -c "import json;print(json.load(open('$1/.agg/state.json'))['$2'])" 2>/dev/null; }

trap '[ -n "${KEEP:-}" ] || rm -rf "$WS"' EXIT
mkdir -p "$WS"

REAL_CLAUDE="$(command -v claude || true)"
[ -x "$REAL_CLAUDE" ] || { echo "claude not on PATH"; exit 1; }

printf '\033[1mAgenticGoGo — REAL-model e2e\033[0m\n'
printf 'model: %s   claude: %s\nworkspace: %s\n' "$MODEL" "$REAL_CLAUDE" "$WS"
printf '\033[33mthis spends real tokens.\033[0m\n'
( cd "$ROOT" && cargo build --quiet ) || { bad "cargo build"; exit 1; }

# ── fixture: a passthrough-instrumented `claude` + a deterministic external judge ─────────
mkproj() { # mkproj <name> <goals.yaml body> <agg.yaml extra> <resume prompt>
  local d="$WS/$1"; mkdir -p "$d/bin" "$d/judges"
  cat > "$d/bin/claude" <<EOF
#!/bin/sh
# PASSTHROUGH: record what agg invoked us with, note the live phase, then run the REAL claude.
for a in "\$@"; do [ "\$a" = "--version" ] && exec "$REAL_CLAUDE" "\$@"; done
printf '%s\n' "\$*" >> claude_args.txt
sh ./rec RUN
exec "$REAL_CLAUDE" "\$@"
EOF
  cat > "$d/rec" <<'EOF'
#!/bin/sh
printf '%s=%s\n' "$1" "$(sed -n 's/.*"phase":"\([a-z]*\)".*/\1/p' .agg/state.json)" >> trace.txt
EOF
  chmod +x "$d/bin/claude" "$d/rec"
  printf '%s' "$2" > "$d/goals.yaml"
  { printf 'project: %s\nmodel: %s\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\n' "$1" "$MODEL"
    printf 'cost: { total: 1.0 }\nhalt_when: over_cost\n'
    printf 'hooks:\n  on_session_start: ["sh ./rec INJECT"]\n  on_session_end: ["sh ./rec GATE"]\n'
    printf '%s' "$3"; } > "$d/agg.yaml"
  printf '%s' "$4" > "$d/AGG_RESUME.md"
  echo "$d"
}
run_agg() { local d=$1; shift; ( cd "$d" && PATH="$d/bin:$PATH" "$AGG" "$@" ); }

# ═════════════════════════════════════════════════════════════════════════════════════════
sec "1. a real agent, driven by agg, satisfying a real external judge"
A="$(mkproj oneshot \
'goals:
  - id: answered
    type: binary
    judge: { kind: script, cmd: "./judges/check.sh" }
stop_when: answered
' '' 'Create a file named `answer.txt` in the current directory whose entire contents are the number 42 followed by a newline. Nothing else. Then stop.
')"
cat > "$A/judges/check.sh" <<'EOF'
#!/bin/sh
sh ./rec VERIFY
if [ -f answer.txt ] && grep -qx '42' answer.txt; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"answer.txt contains 42"}'
else
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"answer.txt missing or wrong"}'
fi
EOF
chmod +x "$A/judges/check.sh"

T0=$(date +%s)
run_agg "$A" run --max-sessions 3 > "$A/out.log" 2>&1
RC=$?
E1=$(( $(date +%s) - T0 ))
printf '  (%ss)\n' "$E1"
is "the loop reaches its stop condition (exit 0)" "$RC" "0"
has "…and says so"                                "$A/out.log" "STOP condition satisfied"
exists "the REAL agent created the file"          "$A/answer.txt"
is "…with exactly the content the goal demands"   "$(cat "$A/answer.txt" 2>/dev/null)" "42"

sec "2. the four outer-loop stages, observed on a real run"
TRACE=$(tr '\n' ' ' < "$A/trace.txt" 2>/dev/null)
printf '  trace: %s\n' "$TRACE"
is "baseline VERIFY → INJECT → RUN → VERIFY → GATE" \
   "$TRACE" "VERIFY=verify INJECT=inject RUN=run VERIFY=verify GATE=gate "
is "the run settles on phase=done" "$(snap "$A" phase)" "done"

sec "3. real worker accounting (what a stub can never prove)"
TOK=$(snap "$A" tokens_spent); COST=$(snap "$A" cost_spent)
printf '  tokens_spent=%s  cost_spent=$%s\n' "$TOK" "$COST"
[ "${TOK:-0}" -gt 0 ] 2>/dev/null && ok "output tokens parsed from the real result event" \
  || bad "tokens_spent is 0 — real usage.output_tokens not read"
python3 -c "import sys;sys.exit(0 if float('${COST:-0}') > 0 else 1)" \
  && ok "dollar cost parsed from the real total_cost_usd" \
  || bad "cost_spent is 0 — real total_cost_usd not read"
has "…and the session-exit line reports both" "$A/out.log" "out-tok"

sec "4. real stream-json parsing"
python3 - "$A" <<'PY'
import json, sys
d = json.load(open(sys.argv[1] + "/.agg/state.json"))
r = d.get("recent", [])
kinds = sorted({e["kind"] for e in r})
print(f"  activity events: {len(r)}  kinds={kinds}")
sys.exit(0 if r and {"think", "tool", "result"} & set(kinds) else 1)
PY
[ $? -eq 0 ] && ok "the reader thread turned real assistant/tool events into the activity tail" \
             || bad "real stream-json events were not parsed into activity"

sec "5. the flags agg actually hands the real CLI"
has "…--dangerously-skip-permissions" "$A/claude_args.txt" "--dangerously-skip-permissions"
has "…--output-format stream-json"    "$A/claude_args.txt" "--output-format stream-json"
has "…--model <the configured model>" "$A/claude_args.txt" "$MODEL"
hasnt "…and NO --resume by default (fresh context per session)" "$A/claude_args.txt" "--resume"

sec "6. durable side effects of a real run"
exists "institutional memory was written" "$A/AGG_MEMORY.md"
has    "…recording the real session"      "$A/AGG_MEMORY.md" "session 1"
is "the ledger is finalized as goals-met" \
   "$(python3 -c "import json;print(json.load(open('$A/.agg/project.json'))['runs'][-1]['end_reason'])" 2>/dev/null)" "goals-met"
[ ! -f "$A/.agg/run.pid" ] && ok "run.pid cleared by the Drop guard" || bad "run.pid left behind"
run_agg "$A" status > "$A/status.log" 2>&1
has "agg status renders the finished real run" "$A/status.log" "done"

# ═════════════════════════════════════════════════════════════════════════════════════════
sec "7. TWO real sessions: --resume continuity + memory carried across the boundary"
# a counter goal: one increment per session, so the loop MUST take two sessions.
B="$(mkproj resume \
'goals:
  - id: counted
    type: binary
    judge: { kind: script, cmd: "./judges/check.sh" }
stop_when: counted
' 'resume_sessions: true
' 'Read the file `count.txt` in the current directory (if it does not exist, treat its value as 0).
Increment that number by exactly ONE. Write the new number back to `count.txt` as the only
contents, followed by a newline. Increment exactly once, then stop. Do not skip ahead.
')"
cat > "$B/judges/check.sh" <<'EOF'
#!/bin/sh
sh ./rec VERIFY
n=$(cat count.txt 2>/dev/null | tr -d '[:space:]')
if [ "$n" = "2" ]; then
  echo '{"met":true,"value":2,"max":2,"target":2,"rationale":"count reached 2"}'
else
  printf '{"met":false,"value":0,"max":2,"target":2,"rationale":"count is %s"}\n' "${n:-0}"
fi
EOF
chmod +x "$B/judges/check.sh"

T0=$(date +%s)
run_agg "$B" run --max-sessions 4 > "$B/out.log" 2>&1
RCB=$?
E2=$(( $(date +%s) - T0 ))
printf '  (%ss)  count.txt=%s\n' "$E2" "$(cat "$B/count.txt" 2>/dev/null | tr -d '\n')"

is "the two-session goal is reached (exit 0)" "$RCB" "0"
SESS=$(snap "$B" session)
[ "${SESS:-0}" -ge 2 ] && ok "…and it genuinely took ≥2 real sessions (session=$SESS)" \
                       || bad "expected ≥2 sessions, got $SESS"
is "the counter really reached 2" "$(tr -d '[:space:]' < "$B/count.txt" 2>/dev/null)" "2"
has "session 2 was launched with --resume (resume_sessions: true)" "$B/claude_args.txt" "--resume"
python3 - "$B" <<'PY'
import re, sys
args = open(sys.argv[1] + "/claude_args.txt").read().splitlines()
ids = [m.group(1) for l in args for m in [re.search(r"--resume (\S+)", l)] if m]
print(f"  --resume ids seen: {ids}")
ok = bool(ids) and all(re.fullmatch(r"[0-9a-f-]{16,}", i) for i in ids)
sys.exit(0 if ok else 1)
PY
[ $? -eq 0 ] && ok "…with a real session_id agg extracted from the prior result event" \
             || bad "the --resume id is not a real session_id"
COUNT_FOLDS=$(grep -c "^## session" "$B/AGG_MEMORY.md" 2>/dev/null || echo 0)
[ "$COUNT_FOLDS" -ge 2 ] && ok "memory folded one entry per real session ($COUNT_FOLDS)" \
                         || bad "expected ≥2 memory entries, got $COUNT_FOLDS"
has "…and the prior session's record was INJECTed into session 2's prompt" "$B/out.log" "[memory] session #1 folded"

TOTAL=$(python3 -c "print(round(float('$(snap "$A" cost_spent)') + float('$(snap "$B" cost_spent)'), 4))" 2>/dev/null)
printf '\n\033[1m══ summary ══\033[0m\n  passed: \033[32m%d\033[0m   failed: \033[31m%d\033[0m\n' "$PASS" "$FAIL"
printf '  real spend: $%s   wall: %ss\n' "${TOTAL:-?}" "$((E1 + E2))"
if [ "$FAIL" -gt 0 ]; then
  printf '\n\033[31mfailures:\033[0m\n'; for f in "${FAILED[@]}"; do printf '  • %s\n' "$f"; done
  exit 1
fi
printf '\n\033[32mall green — against a real model\033[0m\n'
