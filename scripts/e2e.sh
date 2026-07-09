#!/usr/bin/env bash
# Full end-to-end acceptance suite, from a USER's perspective.
#
# Drives the real `agg` binary, the real TUI, the real `agg serve` HTTP API and the real
# SvelteKit web app — no mocks except the `claude` worker itself (a shell stub on PATH that
# emits valid stream-json), because a real model is non-deterministic, costs money and needs
# network. Everything else is the shipping code.
#
#   ./scripts/e2e.sh              # everything
#   ./scripts/e2e.sh --no-web     # skip the SvelteKit app (no node needed)
#   ./scripts/e2e.sh --no-tui     # skip the interactive pty check
#   KEEP=1 ./scripts/e2e.sh       # keep the workspace for inspection
#
# Exits 0 only if every check passed. Unix-only (the stub + pty use sh/script).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WS="${TMPDIR:-/tmp}/agg-e2e.$$"
AGG="$ROOT/target/debug/agg"
WEB=1; TUI=1
for a in "$@"; do
  case "$a" in
    --no-web) WEB=0 ;;
    --no-tui) TUI=0 ;;
    *) echo "unknown flag: $a" >&2; exit 2 ;;
  esac
done

PASS=0; FAIL=0; SKIP=0
declare -a FAILED=()

sec()  { printf '\n\033[1m── %s\033[0m\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf '  \033[32m✔\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); FAILED+=("$1"); printf '  \033[31m✘ %s\033[0m\n' "$1"; [ -n "${2:-}" ] && printf '      %s\n' "$2"; return 0; }
skip() { SKIP=$((SKIP+1)); printf '  \033[33m∼\033[0m %s (skipped: %s)\n' "$1" "$2"; }

# assert helpers -------------------------------------------------------------
is()      { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$3], got [$2]"; }
has()     { grep -qF -- "$3" "$2" 2>/dev/null && ok "$1" || bad "$1" "'$3' not found in $2"; }
hasnt()   { grep -qF -- "$3" "$2" 2>/dev/null && bad "$1" "'$3' unexpectedly present in $2" || ok "$1"; }
exists()  { [ -e "$2" ] && ok "$1" || bad "$1" "missing: $2"; }
absent()  { [ -e "$2" ] && bad "$1" "should not exist: $2" || ok "$1"; }

# Poll until `cmd` succeeds, or fail after N seconds. Polling (not fixed sleeps) is what keeps
# the suite fast AND non-flaky: nothing depends on how long a machine takes, only on the
# condition becoming true. Counts as a real assertion either way.
waitfor() { # waitfor <secs> <desc> <cmd...>
  local secs=$1 desc=$2; shift 2
  local deadline=$(( $(date +%s) + secs ))
  until "$@" 2>/dev/null; do
    [ "$(date +%s)" -ge "$deadline" ] && { bad "$desc" "timed out after ${secs}s"; return 1; }
    sleep 0.1
  done
  ok "$desc"
  return 0
}

free_port()     { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'; }
snap()          { python3 -c "import json;print(json.load(open('$1/.agg/state.json'))['$2'])" 2>/dev/null; }
phase_of()      { snap "$1" phase; }
finish_reason() { snap "$1" finish_reason; }

# strip ANSI/CR so a TUI capture can be grepped
deansi() { LC_ALL=C sed -e $'s/\x1b\[[0-9;?]*[A-Za-z]//g' -e $'s/\x1b[()][AB0]//g' -e 's/\r//g' "$1" > "$2"; }

# ---------------------------------------------------------------------------
# fixture: a project with a fake `claude` on PATH.
#   the worker records the live phase, dumps the prompt it was handed, honours
#   WORKER_SLEEP / NO_WORK / WORKER_TOKENS / WORKER_COST toggle-files.
#   the judge records the live phase and honours JUDGE_FAIL.
# ---------------------------------------------------------------------------
mkproj() { # mkproj <name> [extra agg.yaml lines]
  local name=$1 extra=${2:-}
  local d="$WS/$name"
  mkdir -p "$d/bin" "$d/judges"

  cat > "$d/bin/rec" <<'EOF'
#!/bin/sh
printf '%s=%s\n' "$1" "$(sed -n 's/.*"phase":"\([a-z]*\)".*/\1/p' .agg/state.json)" >> trace.txt
EOF

  cat > "$d/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake-claude 0.0.0"; exit 0; }; done
prev=""
for a in "$@"; do
  [ "$prev" = "-p" ] && { printf '%s' "$a" > prompt_latest.txt; printf '%s\n===8<===\n' "$a" >> prompts.txt; }
  prev="$a"
done
sh bin/rec RUN
[ -f WORKER_SLEEP ] && sleep "$(cat WORKER_SLEEP)"
[ -f NO_WORK ] || : > did_work
tok=1;   [ -f WORKER_TOKENS ] && tok=$(cat WORKER_TOKENS)
cost=0;  [ -f WORKER_COST ]   && cost=$(cat WORKER_COST)
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":%s},"total_cost_usd":%s}\n' "$tok" "$cost"
exit 0
EOF

  cat > "$d/judges/check.sh" <<'EOF'
#!/bin/sh
sh bin/rec VERIFY
if [ -f JUDGE_FAIL ] || [ ! -f did_work ]; then
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'
else
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"did_work present"}'
fi
EOF

  chmod +x "$d/bin/rec" "$d/bin/claude" "$d/judges/check.sh"
  cat > "$d/goals.yaml" <<'EOF'
goals:
  - id: worked
    type: binary
    judge: { kind: script, cmd: "./judges/check.sh" }
stop_when: worked
EOF
  { printf 'project: %s\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\n' "$name"
    printf 'hooks:\n  on_session_start: ["sh bin/rec INJECT"]\n  on_session_end: ["sh bin/rec GATE"]\n'
    [ -n "$extra" ] && printf '%s\n' "$extra"; } > "$d/agg.yaml"
  echo "create the file did_work" > "$d/AGG_RESUME.md"
  echo "$d"
}

# run `agg` inside a project with the fake claude first on PATH
agg_do() { local d=$1; shift; ( cd "$d" && PATH="$d/bin:$PATH" "$AGG" "$@" ); }

# Launch `agg` in the background; stores ITS pid in the variable named by $1.
#
# Two things here are load-bearing:
#  1. `exec` — bash normally replaces the subshell with the last command (so `$!` is that
#     command), but it SKIPS that optimisation whenever a trap is installed, and we install
#     an EXIT trap below. Without `exec`, `$!` would be a background *bash*, which a
#     non-interactive shell starts with SIGINT set to SIG_IGN: `kill -INT $!` would be
#     silently swallowed and `kill $!` would leave `agg` orphaned holding its port.
#  2. assigning to a caller variable rather than echoing a pid — `VAR=$(agg_bg …)` would run
#     the `&` inside a command-substitution subshell, so the job would not be a child of THIS
#     shell and `wait $VAR` would fail with "not a child of this shell" instead of blocking.
agg_bg() { # agg_bg <varname> <dir> <logfile> <args...>
  local __var=$1 d=$2 log=$3; shift 3
  ( cd "$d" && exec env PATH="$d/bin:$PATH" "$AGG" "$@" > "$log" 2>&1 ) &
  printf -v "$__var" '%s' "$!"
  BGPIDS+=("${!__var}")
}

declare -a BGPIDS=()
reap() { for p in "${BGPIDS[@]:-}"; do [ -n "$p" ] && kill -9 "$p" 2>/dev/null; done; }
trap 'rc=$?; reap; [ -n "${KEEP:-}" ] || rm -rf "$WS"; exit $rc' EXIT
mkdir -p "$WS"

printf '\033[1mAgenticGoGo — full e2e acceptance suite\033[0m\n'
printf 'workspace: %s\n' "$WS"

# ═══════════════════════════════════════════════════════════════════════════
sec "0. build"
( cd "$ROOT" && cargo build --quiet ) && ok "cargo build" || { bad "cargo build"; exit 1; }
[ -x "$AGG" ] && ok "agg binary at target/debug/agg" || bad "agg binary missing"

# ═══════════════════════════════════════════════════════════════════════════
sec "1. scaffolding & diagnostics  (init · doctor · plan · judge)"
D="$WS/scaffold"; mkdir -p "$D/bin"
cat > "$D/bin/claude" <<'EOF'
#!/bin/sh
echo "fake-claude 0.0.0"; exit 0
EOF
chmod +x "$D/bin/claude"

agg_do "$D" init > "$D/init.log" 2>&1
is  "agg init exits 0" "$?" "0"
exists "init scaffolds agg.yaml"      "$D/agg.yaml"
exists "init scaffolds goals.yaml"    "$D/goals.yaml"
exists "init scaffolds AGG_RESUME.md" "$D/AGG_RESUME.md"

agg_do "$D" doctor > "$D/doctor.log" 2>&1
is "agg doctor exits 0 on a good setup" "$?" "0"
has "doctor reports claude on PATH" "$D/doctor.log" "claude"

DF="$WS/doctorbad"; mkdir -p "$DF"
echo "project: broken" > "$DF/agg.yaml"     # no goals.yaml
( cd "$DF" && "$AGG" doctor > doctor.log 2>&1 )
[ $? -ne 0 ] && ok "agg doctor exits non-zero on a broken setup" || bad "doctor should flag a broken setup"

P1="$(mkproj plan)"
agg_do "$P1" plan > "$P1/plan.log" 2>&1
is  "agg plan exits 0" "$?" "0"
has "plan prints the scoreboard"     "$P1/plan.log" "worked"
has "plan re-runs judges (not met)"  "$P1/plan.log" "not yet"

agg_do "$P1" judge worked > "$P1/judge.log" 2>&1
is  "agg judge <id> exits 0" "$?" "0"
has "judge prints raw verdict JSON" "$P1/judge.log" '"met"'
agg_do "$P1" judge no_such_goal > "$P1/judge_bad.log" 2>&1
[ $? -ne 0 ] && ok "agg judge <unknown> exits non-zero" || bad "unknown goal id should fail"

# init --folder layout
FD="$WS/folder"; mkdir -p "$FD"
( cd "$FD" && "$AGG" init --folder > init.log 2>&1 )
exists "init --folder puts config in agg/" "$FD/agg/agg.yaml"

# ═══════════════════════════════════════════════════════════════════════════
sec "2. run lifecycle — every exit code a script can branch on"
G="$(mkproj goalsmet)"
agg_do "$G" run --max-sessions 3 > "$G/run.log" 2>&1
is  "goals met → exit 0" "$?" "0"
has "…prints the STOP banner" "$G/run.log" "STOP condition satisfied"
exists "…the worker really ran" "$G/did_work"

M="$(mkproj maxsess)"; : > "$M/NO_WORK"
agg_do "$M" run --max-sessions 2 > "$M/run.log" 2>&1
is  "goals never met, cap hit → exit 4" "$?" "4"
has "…prints the max_sessions banner" "$M/run.log" "reached max_sessions=2"

H="$(mkproj halted)"; : > "$H/NO_WORK"; echo "0.05" > "$H/WORKER_COST"
printf 'goals:\n  - id: worked\n    type: binary\n    judge: { kind: script, cmd: "./judges/check.sh" }\nstop_when: worked\nhalt_when: over_cost\n' > "$H/goals.yaml"
printf 'project: halted\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\ncost: { total: 0 }\n' > "$H/agg.yaml"
agg_do "$H" run --max-sessions 20 > "$H/run.log" 2>&1
is  "cost guard fires → exit 3 (HALT)" "$?" "3"
has "…names the guard"                  "$H/run.log" "over_cost"
hasnt "…did NOT run to the session cap" "$H/run.log" "reached max_sessions"

A="$(mkproj already)"; : > "$A/did_work"
agg_do "$A" run --max-sessions 3 > "$A/run.log" 2>&1
is  "already satisfied at launch → exit 0" "$?" "0"
has "…says so"                             "$A/run.log" "already satisfied at launch"
is  "…zero sessions burned" "$(python3 -c "import json;print(json.load(open('$A/.agg/state.json'))['session'])")" "0"

NC="$WS/noconfig"; mkdir -p "$NC"
( cd "$NC" && "$AGG" run --max-sessions 1 > run.log 2>&1 )
[ $? -eq 1 ] && ok "no config → exit 1 (hard error, distinct from 3/4)" || bad "missing config should exit 1"
has "…with an actionable hint" "$NC/run.log" "agg init"

# ═══════════════════════════════════════════════════════════════════════════
sec "3. the four outer-loop stages are observable (INJECT · RUN · VERIFY · GATE)"
is "stage trace for a full cycle" \
   "$(tr '\n' ' ' < "$G/trace.txt")" \
   "VERIFY=verify INJECT=inject RUN=run VERIFY=verify GATE=gate "
is "baseline-only run never enters a stage" "$(tr '\n' ' ' < "$A/trace.txt")" "VERIFY=verify "
is "final phase is done" "$(phase_of "$G")" "done"

# ═══════════════════════════════════════════════════════════════════════════
sec "4. status · history · dashboard --once  (read the published snapshot)"
agg_do "$G" status > "$G/status.log" 2>&1
is  "agg status exits 0" "$?" "0"
has "…renders the project"  "$G/status.log" "goalsmet"
has "…renders the goal"     "$G/status.log" "worked"

agg_do "$G" status --json > "$G/status.json" 2>&1
python3 -c "import json;d=json.load(open('$G/status.json'));assert d['project']=='goalsmet';assert d['finished'] is True;assert d['phase']=='done'" \
  && ok "agg status --json is machine-readable (project/finished/phase)" || bad "status --json malformed"

agg_do "$G" history > "$G/history.log" 2>&1
is  "agg history exits 0" "$?" "0"
agg_do "$G" history --json > "$G/history.json" 2>&1
python3 -c "
import json;d=json.load(open('$G/history.json'))
runs=d['runs']; assert len(runs)>=1
r=runs[-1]; assert r['end_reason']=='goals-met', r['end_reason']
assert r['sessions']>=1" && ok "agg history --json records end_reason=goals-met" || bad "history --json malformed"

agg_do "$G" dashboard --once > "$G/dash.log" 2>&1
is  "agg dashboard --once exits 0 (headless snapshot)" "$?" "0"
has "…renders project + goal" "$G/dash.log" "worked"

# ═══════════════════════════════════════════════════════════════════════════
sec "5. steering a LIVE loop over the bus  (inject · note · pause/resume · budget · stop)"

# a queued command with no loop running is accepted but warns
Q="$(mkproj prearm)"
agg_do "$Q" inject "pre-armed" > "$Q/q.log" 2>&1
is  "agg inject with no loop exits 0 (pre-arming is legal)" "$?" "0"
has "…but warns that no loop is running" "$Q/q.log" "NO loop is running"

# --- a slow loop we can steer while it runs
S="$(mkproj steer)"; : > "$S/NO_WORK"; echo 2 > "$S/WORKER_SLEEP"
agg_bg LOOP "$S" run.log run --max-sessions 6
waitfor 30 "live loop reaches its first RUN" grep -q "RUN=run" "$S/trace.txt"

agg_do "$S" send note "hello-bus" > /dev/null 2>&1
agg_do "$S" inject "OPERATOR_MARKER_XYZ" > /dev/null 2>&1
waitfor 30 "injected instruction reaches the NEXT worker prompt" grep -q "OPERATOR_MARKER_XYZ" "$S/prompt_latest.txt"
has "…as a HIGH-PRIORITY header"     "$S/prompt_latest.txt" "HIGH-PRIORITY OPERATOR INSTRUCTION"
has "…and the resume prompt survives" "$S/prompt_latest.txt" "create the file did_work"
has "agg send note is logged by the loop" "$S/run.log" "[bus] note: hello-bus"

agg_do "$S" pause > /dev/null 2>&1
waitfor 30 "agg pause parks the loop in INJECT" grep -q "pause → waiting for resume/stop" "$S/run.log"
is  "…and the published phase says inject" "$(phase_of "$S")" "inject"
agg_do "$S" resume > /dev/null 2>&1
waitfor 30 "agg resume continues the loop" grep -q "resume → continuing" "$S/run.log"

agg_do "$S" stop "e2e-stop-reason" > /dev/null 2>&1
waitfor 40 "agg stop ends the loop" bash -c "! kill -0 $LOOP 2>/dev/null"
wait $LOOP; RC=$?
is  "…exit 0 (an operator stop is a clean end)" "$RC" "0"
# the reason is LOGGED as `[bus] stop → …` and STORED as dash.finish_reason (state.json);
# "stopped via bus: …" is never printed to the log.
has "…the loop logs the bus stop"      "$S/run.log" "[bus] stop → e2e-stop-reason"
is  "…and records the finish reason"   "$(finish_reason "$S")" "stopped via bus: e2e-stop-reason"
absent "…run.pid cleared by the Drop guard" "$S/.agg/run.pid"
is  "…ledger finalized as stopped" \
    "$(python3 -c "import json;print(json.load(open('$S/.agg/project.json'))['runs'][-1]['end_reason'])")" "stopped"

# --- budget steering halts a live loop
B="$(mkproj budget)"; : > "$B/NO_WORK"; echo 2 > "$B/WORKER_SLEEP"; echo 500 > "$B/WORKER_TOKENS"
printf 'goals:\n  - id: worked\n    type: binary\n    judge: { kind: script, cmd: "./judges/check.sh" }\nstop_when: worked\nhalt_when: over_budget\n' > "$B/goals.yaml"
agg_bg BLOOP "$B" run.log run --max-sessions 6
waitfor 30 "live loop for budget test reaches RUN" grep -q "RUN=run" "$B/trace.txt"
agg_do "$B" budget 1 > /dev/null 2>&1
waitfor 40 "agg budget <n> halts the running loop" bash -c "! kill -0 $BLOOP 2>/dev/null"
wait $BLOOP; RC=$?
is  "…exit 3 (a guard fired)" "$RC" "3"
has "…names over_budget"      "$B/run.log" "over_budget"

# ═══════════════════════════════════════════════════════════════════════════
sec "6. interrupt (Ctrl-C) — nothing staged, nothing judged, guards run"
I="$(mkproj intr)"; : > "$I/NO_WORK"; echo 30 > "$I/WORKER_SLEEP"
agg_bg ILOOP "$I" run.log run --max-sessions 3
waitfor 30 "loop reaches RUN before we Ctrl-C it" grep -q "RUN=run" "$I/trace.txt"
kill -INT $ILOOP; wait $ILOOP; RC=$?
is  "SIGINT → exit 0 (clean operator stop)" "$RC" "0"
is  "…trace stops at RUN (never judged, never gated)" "$(tr '\n' ' ' < "$I/trace.txt")" "VERIFY=verify INJECT=inject RUN=run "
has "…prints the interrupt banner"       "$I/run.log" "interrupted (SIGINT/SIGTERM)"
hasnt "…no bogus session-exit log line"  "$I/run.log" "exited (code"
absent "…run.pid cleared" "$I/.agg/run.pid"

# ═══════════════════════════════════════════════════════════════════════════
sec "7. detached run + agg stop"
DT="$(mkproj detach)"; : > "$DT/NO_WORK"; echo 2 > "$DT/WORKER_SLEEP"
agg_do "$DT" run --detach --max-sessions 6 > "$DT/detach.log" 2>&1
is  "agg run --detach returns immediately (exit 0)" "$?" "0"
waitfor 30 "…writes .agg/run.pid" test -f "$DT/.agg/run.pid"
exists "…and logs to .agg/run.log" "$DT/.agg/run.log"
waitfor 30 "…the detached loop really runs" grep -q "RUN=run" "$DT/trace.txt"
agg_do "$DT" run --max-sessions 1 > "$DT/second.log" 2>&1
[ $? -ne 0 ] && ok "double-run guard refuses a second loop" || bad "a second concurrent loop was allowed"
has "…and says which pid holds it" "$DT/second.log" "already running"
agg_do "$DT" stop "detached-stop" > /dev/null 2>&1
waitfor 40 "agg stop ends the detached loop" bash -c "! test -f '$DT/.agg/run.pid'"
ok "…run.pid cleared after the detached loop exits"

# ═══════════════════════════════════════════════════════════════════════════
sec "8. agg spawn — long tasks that outlive a session"
SP="$(mkproj spawn)"
agg_do "$SP" spawn --name e2e-task --reason "long sim" -- sleep 20 > "$SP/spawn.log" 2>&1
is  "agg spawn exits 0" "$?" "0"
exists "…registers the task in .agg/spawns.json" "$SP/.agg/spawns.json"
python3 -c "
import json;d=json.load(open('$SP/.agg/spawns.json'))
e=[x for x in d['spawns'] if x['name']=='e2e-task']
assert e, 'task not registered'
assert e[0]['status']=='running', e[0]['status']
assert 'long sim' in e[0]['reason']" && ok "…status=running with the operator's reason" || bad "spawns.json malformed"
agg_do "$SP" run --max-sessions 1 > "$SP/run.log" 2>&1
has "…the next session's prompt is told about it" "$SP/prompt_latest.txt" "e2e-task"
has "…including WHY, so it polls instead of relaunching" "$SP/prompt_latest.txt" "long sim"
# kill exactly the pid we registered — never a blanket pkill of the user's processes
SPID=$(python3 -c "import json;print([x for x in json.load(open('$SP/.agg/spawns.json'))['spawns'] if x['name']=='e2e-task'][0]['pid'])" 2>/dev/null || true)
[ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null || true

# ═══════════════════════════════════════════════════════════════════════════
sec "9. institutional memory — AGG_MEMORY.md, written without worker cooperation"
MEM="$(mkproj memory)"
agg_do "$MEM" run --max-sessions 2 > "$MEM/run.log" 2>&1
exists "AGG_MEMORY.md is written" "$MEM/AGG_MEMORY.md"
has "…records the session mechanically" "$MEM/AGG_MEMORY.md" "session"
N=$(grep -c "^## session" "$MEM/AGG_MEMORY.md" 2>/dev/null || echo 0)
[ "$N" = "1" ] && ok "…exactly ONE entry per completed session (early fold superseded)" \
               || bad "expected 1 folded entry, got $N"
absent "…scratch note deleted after folding" "$MEM/.agg/memory/session-1.md"

# NO_WORK: run 1 must NOT meet the goal, else run 2 stops at baseline and never builds a prompt.
MEM2="$(mkproj memory2)"; : > "$MEM2/NO_WORK"
agg_do "$MEM2" run --max-sessions 1 > "$MEM2/run1.log" 2>&1
agg_do "$MEM2" run --max-sessions 1 > "$MEM2/run2.log" 2>&1
has "…memory is INJECTed into the NEXT run's prompt" "$MEM2/prompt_latest.txt" "INSTITUTIONAL MEMORY"
has "…carrying the prior session's record across runs" "$MEM2/prompt_latest.txt" "session 1"

# ═══════════════════════════════════════════════════════════════════════════
sec "10. agg serve — the JSON API the web UI depends on"
PORT=$(free_port)
SV="$(mkproj serve)"; : > "$SV/NO_WORK"; echo 3 > "$SV/WORKER_SLEEP"
agg_bg SRV "$SV" serve.log serve --port "$PORT" --cors-origin "http://localhost:5173"
waitfor 20 "agg serve binds 127.0.0.1:$PORT" bash -c "curl -sf http://127.0.0.1:$PORT/api/health >/dev/null"

curl -sf "http://127.0.0.1:$PORT/api/health" -o "$SV/health.json"
python3 -c "import json;d=json.load(open('$SV/health.json'));assert d['running'] is False and d['pid'] is None" \
  && ok "GET /api/health → running:false when no loop" || bad "health wrong with no loop"

C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/send" -d '{"cmd":"pause"}')
is "POST /api/send → 409 when no loop is running" "$C" "409"
C=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/api/nope")
is "GET /api/nope → 404" "$C" "404"
CORS=$(curl -s -D- -o /dev/null "http://127.0.0.1:$PORT/api/health" | tr -d '\r' | sed -n 's/^Access-Control-Allow-Origin: //Ip')
is "…CORS is locked to the configured origin" "$CORS" "http://localhost:5173"

# now start a loop in the same project and re-check
agg_bg SLOOP "$SV" run.log run --max-sessions 6
waitfor 30 "loop is live for the serve tests" grep -q "RUN=run" "$SV/trace.txt"

curl -sf "http://127.0.0.1:$PORT/api/health" -o "$SV/health2.json"
python3 -c "import json;d=json.load(open('$SV/health2.json'));assert d['running'] is True and isinstance(d['pid'],int)" \
  && ok "GET /api/health → running:true + pid once the loop is up" || bad "health wrong with a live loop"

curl -sf "http://127.0.0.1:$PORT/api/state" -o "$SV/state.json"
python3 -c "
import json;d=json.load(open('$SV/state.json'))
assert d['project']=='serve', d['project']
assert d['phase'] in ('inject','run','verify','gate'), d['phase']
assert isinstance(d['goals'],list) and d['goals'][0]['id']=='worked'" \
  && ok "GET /api/state → live snapshot with a four-stage phase" || bad "/api/state malformed"

curl -sf "http://127.0.0.1:$PORT/api/history" -o "$SV/hist.json"
python3 -c "import json;d=json.load(open('$SV/hist.json'));assert 'runs' in d" \
  && ok "GET /api/history → the run ledger" || bad "/api/history malformed"

# Sample the live phase across several cycles rather than at one instant — a single sample can
# miss a renamed stage by luck, which is exactly the regression this guards.
: > "$SV/phases.txt"
SDL=$(( $(date +%s) + 8 ))
while [ "$(date +%s)" -lt "$SDL" ]; do
  # awk, not sed: curl's body has no trailing newline, so `sed …p` would append every sample
  # onto one line (runrunverifyinject…). awk's print always terminates the record.
  curl -sf "http://127.0.0.1:$PORT/api/state" 2>/dev/null \
    | awk 'match($0, /"phase":"[a-z]+"/) { print substr($0, RSTART+9, RLENGTH-10) }' >> "$SV/phases.txt"
  sleep 0.1
done
sort -u "$SV/phases.txt" | grep -v '^$' > "$SV/phases.uniq"
UNKNOWN=$(grep -vE '^(inject|run|verify|gate|backoff|starting|done)$' "$SV/phases.uniq" | tr '\n' ' ')
[ -z "$UNKNOWN" ] && ok "…/api/state only ever exposes known phases" \
                  || bad "/api/state exposed an unknown phase" "saw: $UNKNOWN"
grep -qE '^(inject|run|verify|gate)$' "$SV/phases.uniq" \
  && ok "…and at least one of the four stages is observed live" \
  || bad "no four-stage phase ever observed" "saw: $(tr '\n' ' ' < "$SV/phases.uniq")"
hasnt "…the retired 'judging' phase never appears" "$SV/phases.uniq" "judging"
hasnt "…the retired 'running' phase never appears" "$SV/phases.uniq" "running"

C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/send" -d '{"cmd":"inject","text":"WEB_MARKER_ABC"}')
is "POST /api/send inject → 200" "$C" "200"
waitfor 40 "…and the instruction reaches the worker's next prompt" grep -q "WEB_MARKER_ABC" "$SV/prompt_latest.txt"

C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/send" -d '{"cmd":"inject","text":"  "}')
is "POST /api/send with empty inject text → 400" "$C" "400"
C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/send" -d 'not json')
is "POST /api/send with bad JSON → 400" "$C" "400"

C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/send" -d '{"cmd":"stop","reason":"from-api"}')
is "POST /api/send stop → 200" "$C" "200"
waitfor 40 "…the loop actually stops" bash -c "! kill -0 $SLOOP 2>/dev/null"
wait $SLOOP; is "…with exit 0" "$?" "0"
is "…for the reason the API gave" "$(finish_reason "$SV")" "stopped via bus: from-api"
kill $SRV 2>/dev/null; wait $SRV 2>/dev/null

# auth: --token must be enforced
PORT2=$(free_port)
agg_bg SRV2 "$SV" serve2.log serve --port "$PORT2" --token "s3cret"
waitfor 20 "agg serve --token binds" bash -c "curl -s -o /dev/null http://127.0.0.1:$PORT2/api/health"
C=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT2/api/health")
is "…no bearer token → 401" "$C" "401"
C=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer s3cret" "http://127.0.0.1:$PORT2/api/health")
is "…correct bearer token → 200" "$C" "200"
C=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer wrong" "http://127.0.0.1:$PORT2/api/health")
is "…wrong bearer token → 401" "$C" "401"
kill $SRV2 2>/dev/null; wait $SRV2 2>/dev/null

# ═══════════════════════════════════════════════════════════════════════════
sec "11. the TUI (driven on a real, sized pty)"
if [ "$TUI" = "0" ]; then
  skip "interactive TUI" "--no-tui"
else
  DRIVE="$ROOT/scripts/tui_drive.py"   # `script(1)` gives no window size → ratatui paints 0 cells
  T="$(mkproj tuidemo)"; agg_do "$T" run --max-sessions 1 > "$T/run.log" 2>&1

  ( cd "$T" && python3 "$DRIVE" --key q --after 1.5 --timeout 25 -- "$AGG" dashboard > tui.raw 2>&1 )
  is "TUI launches on a pty and quits on 'q'" "$?" "0"
  deansi "$T/tui.raw" "$T/tui.txt"
  has "…paints the project name" "$T/tui.txt" "tuidemo"
  has "…paints the goal"         "$T/tui.txt" "worked"
  has "…paints the phase field"  "$T/tui.txt" "phase"
  has "…paints the finished banner" "$T/tui.txt" "FINISHED"

  ( cd "$T" && python3 "$DRIVE" --key x --after 1.0 --timeout 8 -- "$AGG" dashboard > /dev/null 2>&1 )
  is "…and does NOT quit on some other key" "$?" "124"

  # a LIVE loop must render one of the four stage names, not the old vocabulary
  TL="$(mkproj tuilive)"; : > "$TL/NO_WORK"; echo 5 > "$TL/WORKER_SLEEP"
  agg_bg TLP "$TL" run.log run --max-sessions 3
  waitfor 30 "live loop for the TUI" grep -q "RUN=run" "$TL/trace.txt"
  ( cd "$TL" && python3 "$DRIVE" --key q --after 1.5 --timeout 25 -- "$AGG" dashboard > tui.raw 2>&1 )
  deansi "$TL/tui.raw" "$TL/tui.txt"
  grep -Eq "phase +(inject|run|verify|gate)" "$TL/tui.txt" \
    && ok "…a live loop renders a four-stage phase (inject/run/verify/gate)" \
    || bad "TUI phase is not one of inject/run/verify/gate" "$(grep -o 'phase [a-z]*' "$TL/tui.txt" | head -1)"
  hasnt "…and never the old 'judging' vocabulary" "$TL/tui.txt" "judging"
  kill -INT $TLP 2>/dev/null; wait $TLP 2>/dev/null
fi

# ═══════════════════════════════════════════════════════════════════════════
sec "12. the web interface (SvelteKit BFF → agg serve → .agg/)"
if [ "$WEB" = "0" ]; then
  skip "web interface" "--no-web"
elif ! command -v node >/dev/null 2>&1; then
  skip "web interface" "node not installed"
elif [ ! -d "$ROOT/web/node_modules" ]; then
  skip "web interface" "run 'npm install' in web/ first"
else
  WPORT=$(free_port); APORT=$(free_port)
  W="$(mkproj web)"; : > "$W/NO_WORK"; echo 3 > "$W/WORKER_SLEEP"

  ( cd "$ROOT/web" && npm run build > "$W/build.log" 2>&1 )
  is "web app builds (npm run build)" "$?" "0"

  agg_bg WSRV "$W" serve.log serve --port "$APORT" --cors-origin "http://localhost:$WPORT"
  ( cd "$ROOT/web" && exec env AGG_API="http://127.0.0.1:$APORT" PORT="$WPORT" node build/index.js > "$W/web.log" 2>&1 ) & WAPP=$!
  BGPIDS+=("$WAPP")
  waitfor 30 "web app serves on :$WPORT" bash -c "curl -sf http://127.0.0.1:$WPORT/ -o /dev/null"

  curl -sf "http://127.0.0.1:$WPORT/" -o "$W/page.html"
  has "…SSR page renders the app shell"   "$W/page.html" "AgenticGoGo"
  has "…and the control buttons"          "$W/page.html" "Pause"
  has "…including Inject/Budget/Stop"     "$W/page.html" "Stop"

  # BFF endpoints with NO loop running
  curl -sf "http://127.0.0.1:$WPORT/api/health" -o "$W/h1.json"
  python3 -c "import json;d=json.load(open('$W/h1.json'));assert d.get('running') is False" \
    && ok "BFF /api/health proxies agg → running:false" || bad "BFF health wrong (no loop)"

  # start a loop; BFF must see it
  agg_bg WLOOP "$W" run.log run --max-sessions 6
  waitfor 30 "loop is live for the web tests" grep -q "RUN=run" "$W/trace.txt"

  waitfor 20 "BFF /api/health → running:true" bash -c "curl -sf http://127.0.0.1:$WPORT/api/health | grep -q '\"running\":true'"
  curl -sf "http://127.0.0.1:$WPORT/api/state" -o "$W/s.json"
  python3 -c "
import json;d=json.load(open('$W/s.json'))
assert d['project']=='web', d
assert d['phase'] in ('inject','run','verify','gate'), d['phase']" \
    && ok "BFF /api/state carries a four-stage phase to the browser" || bad "BFF state malformed"

  curl -sf "http://127.0.0.1:$WPORT/api/history" -o "$W/hi.json"
  python3 -c "import json;json.load(open('$W/hi.json'))" && ok "BFF /api/history proxies the ledger" || bad "BFF history malformed"

  # the Controls.svelte buttons post exactly these bodies
  C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$WPORT/api/send" -H 'content-type: application/json' -d '{"cmd":"inject","text":"WEB_UI_MARKER"}')
  is "web ✎ Inject → BFF → agg (200)" "$C" "200"
  waitfor 40 "…reaches the worker's next prompt" grep -q "WEB_UI_MARKER" "$W/prompt_latest.txt"

  C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$WPORT/api/send" -H 'content-type: application/json' -d '{"cmd":"pause"}')
  is "web ⏸ Pause → 200" "$C" "200"
  waitfor 30 "…the loop parks" grep -q "pause → waiting" "$W/run.log"
  C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$WPORT/api/send" -H 'content-type: application/json' -d '{"cmd":"resume"}')
  is "web ▶ Resume → 200" "$C" "200"
  waitfor 30 "…the loop continues" grep -q "resume → continuing" "$W/run.log"

  C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$WPORT/api/send" -H 'content-type: application/json' -d '{"cmd":"budget","total":null}')
  is "web ◫ Budget (unlimited) → 200" "$C" "200"

  C=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$WPORT/api/send" -H 'content-type: application/json' -d '{"cmd":"stop","reason":"stopped from web"}')
  is "web ⏹ Stop → 200" "$C" "200"
  waitfor 40 "…the loop really stops" bash -c "! kill -0 $WLOOP 2>/dev/null"
  wait $WLOOP; is "…exit 0" "$?" "0"
  is "…with the web's reason" "$(finish_reason "$W")" "stopped via bus: stopped from web"

  waitfor 20 "BFF /api/health → running:false after the stop" bash -c "curl -sf http://127.0.0.1:$WPORT/api/health | grep -q '\"running\":false'"

  kill $WAPP 2>/dev/null; wait $WAPP 2>/dev/null
  kill $WSRV 2>/dev/null; wait $WSRV 2>/dev/null

  # the BFF must degrade gracefully when agg serve is gone (api_offline path)
  ( cd "$ROOT/web" && exec env AGG_API="http://127.0.0.1:1" PORT="$WPORT" node build/index.js > "$W/web2.log" 2>&1 ) & WAPP2=$!
  BGPIDS+=("$WAPP2")
  waitfor 30 "web app restarts with agg serve DOWN" bash -c "curl -sf http://127.0.0.1:$WPORT/ -o /dev/null"
  curl -s "http://127.0.0.1:$WPORT/api/health" -o "$W/h2.json"
  python3 -c "
import json;d=json.load(open('$W/h2.json'))
assert d.get('api_offline') is True or d.get('running') is False, d" \
    && ok "BFF reports api_offline instead of 500ing when agg serve is down" || bad "BFF does not degrade gracefully"
  kill $WAPP2 2>/dev/null; wait $WAPP2 2>/dev/null
fi

# ═══════════════════════════════════════════════════════════════════════════
printf '\n\033[1m══ summary ══\033[0m\n'
printf '  passed: \033[32m%d\033[0m   failed: \033[31m%d\033[0m   skipped: \033[33m%d\033[0m\n' "$PASS" "$FAIL" "$SKIP"
if [ "$FAIL" -gt 0 ]; then
  printf '\n\033[31mfailures:\033[0m\n'
  for f in "${FAILED[@]}"; do printf '  • %s\n' "$f"; done
  exit 1
fi
printf '\n\033[32mall green\033[0m\n'
