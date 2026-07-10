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
sec "9b. git session isolation + the rollback GATE"
# NOTE: agg discards a session branch that has UNCOMMITTED edits ("commit your work to keep it"),
# so the worker must commit — that is the real contract with the worker, not a test artifact.
# The repo must also be clean when `agg run` starts, so every fixture file is committed first.
mkrepo() { # mkrepo <dir>  — commit everything the fixture made so far, on `main`
  ( cd "$1" && git init -q -b main && git config user.email e@e && git config user.name e \
    && printf 'did_work\ntrace.txt\nprompt*.txt\nargv.txt\n.sess\n.n\nNO_WORK\nWORKER_SLEEP\nWORKER_TOKENS\nWORKER_COST\nJUDGE_FAIL\nrun.log\nAGG_MEMORY.md\nAGG_RED\n' > .gitignore \
    && echo base > tracked.txt && git add -A && git commit -qm init )
}

GI="$(mkproj iso)"
cat > "$GI/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
echo worker-edit > tracked.txt
git add -A && git commit -qm "worker: session work"   # the worker commits on its session branch
: > did_work
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$GI/bin/claude"
printf 'project: iso\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nsession_isolation: { enabled: true }\nhooks:\n  on_session_start: ["sh bin/rec INJECT"]\n  on_session_end: ["sh bin/rec GATE"]\n' > "$GI/agg.yaml"
mkrepo "$GI"
agg_do "$GI" run --max-sessions 1 > "$GI/run.log" 2>&1
has "isolation cuts a per-session branch off the base"  "$GI/run.log" "[iso] session #1 on branch"
has "…and a green session is MERGED back"               "$GI/run.log" "merged → kept"
is  "…so the worker's commit is on base" \
    "$( cd "$GI" && git show HEAD:tracked.txt 2>/dev/null )" "worker-edit"

# now regress a previously-met goal → the GATE must roll the merge back.
# a second, never-met goal keeps the loop alive past session 1.
GR="$(mkproj rollback)"
cat > "$GR/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
n=$(cat .sess 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > .sess
echo "sess-$n" > tracked.txt
git add -A && git commit -qm "worker: session $n"
: > did_work
[ "$n" -ge 2 ] && : > JUDGE_FAIL   # session 2 REGRESSES the goal session 1 had met
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$GR/bin/claude"
cat > "$GR/judges/never.sh" <<'EOF'
#!/bin/sh
echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"keeps the loop alive"}'
EOF
chmod +x "$GR/judges/never.sh"
printf 'goals:\n  - id: worked\n    type: binary\n    judge: { kind: script, cmd: "./judges/check.sh" }\n  - id: endless\n    type: binary\n    judge: { kind: script, cmd: "./judges/never.sh" }\nstop_when: endless\n' > "$GR/goals.yaml"
printf 'project: rollback\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nsession_isolation: { enabled: true, rollback_on_regression: true }\n' > "$GR/agg.yaml"
mkrepo "$GR"
agg_do "$GR" run --max-sessions 2 > "$GR/run.log" 2>&1
has "session 1 (green) is merged onto base"          "$GR/run.log" "session #1 merged → kept"
has "session 2 (regressing) is ROLLED BACK"          "$GR/run.log" "session #2 ROLLED BACK"
is  "…and its work NEVER lands on base (base still holds session 1)" \
    "$( cd "$GR" && git show HEAD:tracked.txt 2>/dev/null )" "sess-1"
has "…the durable memory says the work is NOT on base" "$GR/AGG_MEMORY.md" "NOT on the base branch"
has "…and the session branch is kept for inspection"   "$GR/run.log" "kept for inspection"

# the worker's own veto: writing the red file discards the session, merged or not
GV="$(mkproj veto)"
cat > "$GV/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
echo vetoed-work > tracked.txt
git add -A && git commit -qm "worker: work I do not trust"
: > AGG_RED            # the worker vetoes its own session
: > did_work
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$GV/bin/claude"
printf 'project: veto\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nsession_isolation: { enabled: true, red_file: "AGG_RED" }\n' > "$GV/agg.yaml"
mkrepo "$GV"
agg_do "$GV" run --max-sessions 1 > "$GV/run.log" 2>&1
has "a worker that writes the red file VETOES its own session" "$GV/run.log" "VETOED"
is  "…and none of its work reaches base" \
    "$( cd "$GV" && git show HEAD:tracked.txt 2>/dev/null )" "base"

# ═══════════════════════════════════════════════════════════════════════════
sec "9c. worker-failure paths (rate-limit backoff · hung-worker watchdog)"
RL="$(mkproj ratelimit)"
# `worker.rs`: "a clean exit 0 is never a rate-limit, even if a transient event looked like one" —
# detection is exit-code AND terminal-event gated, so the stub must also exit non-zero.
cat > "$RL/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
printf '{"type":"result","subtype":"error","is_error":true,"result":"rate_limit_error: slow down","usage":{"output_tokens":0},"total_cost_usd":0}\n'
exit 1
EOF
chmod +x "$RL/bin/claude"
printf 'project: ratelimit\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nratelimit_backoff_secs: 1\nhooks:\n  on_session_start: ["sh bin/rec INJECT"]\n  on_session_end: ["sh bin/rec GATE"]\n' > "$RL/agg.yaml"
agg_do "$RL" run --max-sessions 2 > "$RL/run.log" 2>&1
has "a rate-limited session backs off"        "$RL/run.log" "rate limit detected"
has "…and is flagged on the exit line"        "$RL/run.log" "[RATE-LIMITED]"
hasnt "…and is NEVER judged"                  "$RL/run.log" "running judges…"
absent "…and leaves NO durable memory entry"  "$RL/AGG_MEMORY.md"
is "…the trace shows no VERIFY/GATE after RUN" \
   "$(tr '\n' ' ' < "$RL/trace.txt")" "VERIFY=verify INJECT=inject RUN=run INJECT=inject RUN=run "

# The watchdog polls every 30s, so even with idle_secs=3 the kill lands ~90s in. This is the
# check that caught `parse_ps_time` rejecting macOS's fractional `ps` TIME ("0:00.00"), which made
# cpu_jiffies() return -1 forever and silently disabled the CPU-flat detector on every mac.
if [ -n "${SKIP_SLOW:-}" ]; then
  skip "hung-worker watchdog" "SKIP_SLOW=1 (it must wait ~90s for the 30s watchdog poll)"
else
WD="$(mkproj watchdog)"
cat > "$WD/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
sleep 300      # stream-idle and cpu-flat: exactly the hang the watchdog exists to kill
EOF
chmod +x "$WD/bin/claude"
printf 'project: watchdog\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nwatchdog: { idle_secs: 3, cpu_grace: 2 }\n' > "$WD/agg.yaml"
: > "$WD/NO_WORK"
WDS=$(date +%s)
agg_do "$WD" run --max-sessions 1 > "$WD/run.log" 2>&1
is "a hung worker is SIGKILLed and the loop survives (exit 4)" "$?" "4"
has "…the watchdog announces the SIGKILL"    "$WD/run.log" "WATCHDOG: worker pid"
has "…and flags it on the session exit line" "$WD/run.log" "WATCHDOG-KILLED"
[ $(( $(date +%s) - WDS )) -lt 200 ] \
  && ok "…and it fires promptly, not after the worker finishes on its own" \
  || bad "watchdog did not fire (the worker ran to completion)"
fi

# ═══════════════════════════════════════════════════════════════════════════
sec "9d. prompt composition (prompt_includes · --resume) and lifecycle hooks"
PI="$(mkproj promptinc)"; : > "$PI/NO_WORK"
echo "TOOLING_FRAGMENT_ZZZ" > "$PI/frag.md"
cat > "$PI/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
prev=""; for a in "$@"; do
  [ "$prev" = "-p" ] && printf '%s' "$a" > prompt_latest.txt
  [ "$prev" = "--resume" ] && echo "$a" >> resumed_with.txt
  prev="$a"
done
sh bin/rec RUN
printf '{"type":"result","subtype":"success","is_error":false,"session_id":"sess-abc-123","result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$PI/bin/claude"
printf 'project: promptinc\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nresume_sessions: true\nprompt_includes: ["frag.md"]\nhooks:\n  on_start: ["echo HOOK_ON_START"]\n  on_stop: ["echo HOOK_ON_STOP"]\n  background: ["sleep 30"]\n  on_session_start: ["sh bin/rec INJECT"]\n  on_session_end: ["sh bin/rec GATE"]\n' > "$PI/agg.yaml"
agg_do "$PI" run --max-sessions 2 > "$PI/run.log" 2>&1
has "prompt_includes are prepended to every prompt" "$PI/prompt_latest.txt" "TOOLING_FRAGMENT_ZZZ"
has "…above the resume prompt"                      "$PI/prompt_latest.txt" "create the file did_work"
has "resume_sessions passes --resume <session_id>"  "$PI/resumed_with.txt" "sess-abc-123"
has "on_start hook runs once at launch"             "$PI/run.log" "HOOK_ON_START"
has "on_stop hook runs on exit (Drop guard)"        "$PI/run.log" "HOOK_ON_STOP"
has "background hook is spawned"                    "$PI/run.log" "[hook:background]"

# ═══════════════════════════════════════════════════════════════════════════
sec "9e. LLM-backed pieces (llm judge · summarizer) against a stubbed model"
LJ="$(mkproj llmjudge)"
# `--output-format json` marks the judge/summary calls; the worker uses stream-json.
cat > "$LJ/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
fmt=""; prompt=""; prev=""
for a in "$@"; do
  [ "$prev" = "--output-format" ] && fmt="$a"
  [ "$prev" = "-p" ] && prompt="$a"
  prev="$a"
done
if [ "$fmt" = "json" ]; then
  case "$prompt" in
    *cumulative*) printf '{"result":"{\\"cumulative\\":\\"CUMULATIVE_SUMMARY_X\\",\\"windowed\\":\\"WINDOWED_SUMMARY_Y\\"}"}\n' ;;
    # the llm judge must be NOT-met at baseline, else the loop stops before running a session
    *) if [ -f did_work ]; then
         printf '{"result":"{\\"met\\":true,\\"value\\":1,\\"max\\":1,\\"target\\":1,\\"rationale\\":\\"LLM_JUDGE_SAYS_OK\\"}"}\n'
       else
         printf '{"result":"{\\"met\\":false,\\"value\\":0,\\"max\\":1,\\"target\\":1,\\"rationale\\":\\"LLM_JUDGE_SAYS_NOT_YET\\"}"}\n'
       fi ;;
  esac
  exit 0
fi
sh bin/rec RUN
: > did_work
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$LJ/bin/claude"
echo "Decide whether the work is done." > "$LJ/rubric.md"
printf 'goals:\n  - id: reviewed\n    type: binary\n    judge: { kind: llm, model: fake, rubric: "rubric.md", inputs: [] }\nstop_when: reviewed\n' > "$LJ/goals.yaml"
printf 'project: llmjudge\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: true, model: fake, min_interval_secs: 0 }\n' > "$LJ/agg.yaml"
agg_do "$LJ" run --max-sessions 2 > "$LJ/run.log" 2>&1
is  "an llm judge drives the loop to its stop condition (exit 0)" "$?" "0"
has "…it reports not-met at baseline"          "$LJ/run.log" "LLM_JUDGE_SAYS_NOT_YET"
has "…then met after the worker ran"           "$LJ/run.log" "LLM_JUDGE_SAYS_OK"
has "the summarizer runs and logs a cumulative summary" "$LJ/run.log" "CUMULATIVE_SUMMARY_X"
has "…and a windowed summary"                           "$LJ/run.log" "WINDOWED_SUMMARY_Y"
is  "…and the summary is published to state.json" "$(snap "$LJ" summary_cumulative)" "CUMULATIVE_SUMMARY_X"

# ═══════════════════════════════════════════════════════════════════════════
sec "9f. worker_args · goal types · over_iterations · wall_hours"

# ── worker_args: extra flags agg must hand the worker, in the right POSITION ──────────────
# There is NO agg log line for worker_args (worker.rs:73 just appends them), so the only
# honest observation channel is the worker recording its own argv. Asserting on run.log
# would pass for the wrong reason.
WA="$(mkproj workerargs)"
cat > "$WA/bin/claude" <<'EOF'
#!/bin/sh
# the --version preflight must exit BEFORE we record, or it overwrites argv.txt with 1 token
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
: > argv.txt; for a in "$@"; do printf '%s\n' "$a" >> argv.txt; done
sh bin/rec RUN
: > did_work
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$WA/bin/claude"
printf 'project: workerargs\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nworker_args: ["--allowedTools", "Edit,Bash", "--add-dir", "SENTINEL_SRC"]\nhooks:\n  on_session_start: ["sh bin/rec INJECT"]\n  on_session_end: ["sh bin/rec GATE"]\n' > "$WA/agg.yaml"
agg_do "$WA" run --max-sessions 2 > "$WA/run.log" 2>&1
is  "worker_args: the run still succeeds" "$?" "0"
has "…--allowedTools reached the worker" "$WA/argv.txt" "--allowedTools"
has "…with its value"                    "$WA/argv.txt" "Edit,Bash"
has "…--add-dir reached the worker"      "$WA/argv.txt" "SENTINEL_SRC"
# POSITION is the real contract: after agg's own flags, before -p (else claude folds them
# into the prompt). Anchor to --output-format, not --verbose: `--effort` sits in between.
python3 - "$WA/argv.txt" <<'PY'
import sys
a = open(sys.argv[1]).read().split("\n")
i_fmt, i_wa, i_p = a.index("--output-format"), a.index("--allowedTools"), a.index("-p")
sys.exit(0 if i_fmt < i_wa < i_p else 1)
PY
[ $? -eq 0 ] && ok "…and they sit AFTER agg's flags and BEFORE -p" \
             || bad "worker_args are in the wrong argv position"

# ── percentage + cardinal goal types ─────────────────────────────────────────────────────
# `met` comes from the judge's verdict (model.rs:185); `value` only decides InProgress vs
# Pending. So the judge must report met:false with a rising value, then met:true at target.
GT="$(mkproj goaltypes)"
cat > "$GT/judges/pct.sh" <<'EOF'
#!/bin/sh
sh bin/rec VERIFY
n=$(cat .n 2>/dev/null || echo 0)
if [ "$n" -ge 1 ]; then echo '{"met":true,"value":100,"max":100,"target":100,"rationale":"done"}'
else echo '{"met":false,"value":50,"max":100,"target":100,"rationale":"halfway"}'; fi
EOF
cat > "$GT/judges/card.sh" <<'EOF'
#!/bin/sh
n=$(cat .n 2>/dev/null || echo 0)
if [ "$n" -ge 1 ]; then echo '{"met":true,"value":28,"max":28,"target":28,"rationale":"all 28"}'
else echo '{"met":false,"value":18,"max":28,"target":28,"rationale":"18 of 28"}'; fi
EOF
chmod +x "$GT/judges/pct.sh" "$GT/judges/card.sh"
cat > "$GT/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
echo 1 > .n
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$GT/bin/claude"
printf 'goals:\n  - id: coverage\n    type: percentage\n    target: 100\n    judge: { kind: script, cmd: "./judges/pct.sh" }\n  - id: solved\n    type: cardinal\n    target: 28\n    judge: { kind: script, cmd: "./judges/card.sh" }\nstop_when: coverage AND solved\n' > "$GT/goals.yaml"
printf 'project: goaltypes\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nhooks:\n  on_session_start: ["sh bin/rec INJECT"]\n  on_session_end: ["sh bin/rec GATE"]\n' > "$GT/agg.yaml"
agg_do "$GT" run --max-sessions 3 > "$GT/run.log" 2>&1
is  "percentage+cardinal goals drive the loop to stop (exit 0)" "$?" "0"
has "…baseline renders the percentage measure" "$GT/run.log" "50/100%"
has "…and the cardinal measure"                "$GT/run.log" "18/28"
has "…the percentage goal reaches target"      "$GT/run.log" "100/100%"
has "…and the cardinal goal reaches target"    "$GT/run.log" "28/28"
has "…the compound stop_when (AND) fires"      "$GT/run.log" "2/2 goals met"

# ── over_iterations: a GUARD (exit 3), distinct from the max-sessions cap (exit 4) ───────
# stop.rs:355 — sessions_done >= max_sessions. It is evaluated in GATE, so it halts BEFORE
# the loop's own top-of-cycle max-sessions pre-check ever fires.
OI="$(mkproj overiter)"; : > "$OI/NO_WORK"
printf 'goals:\n  - id: worked\n    type: binary\n    judge: { kind: script, cmd: "./judges/check.sh" }\nstop_when: worked\nhalt_when: over_iterations\n' > "$OI/goals.yaml"
agg_do "$OI" run --max-sessions 2 > "$OI/run.log" 2>&1
is    "over_iterations HALTS the loop (exit 3, a guard — not the exit-4 cap)" "$?" "3"
has   "…and names the guard"                 "$OI/run.log" "over_iterations"
hasnt "…the max-sessions cap never fired"    "$OI/run.log" "reached max_sessions"

# ── wall_hours: a raw counter usable in any halt expression (stop.rs:338) ────────────────
WH="$(mkproj wallhours)"; : > "$WH/NO_WORK"; echo 4 > "$WH/WORKER_SLEEP"
# baseline sits at ~0.00001h; one 4s session puts wall_hours at ~0.0012h.
printf 'goals:\n  - id: worked\n    type: binary\n    judge: { kind: script, cmd: "./judges/check.sh" }\nstop_when: worked\nhalt_when: wall_hours >= 0.0005\n' > "$WH/goals.yaml"
agg_do "$WH" run --max-sessions 5 > "$WH/run.log" 2>&1
is    "a wall_hours ceiling HALTS the loop (exit 3)" "$?" "3"
has   "…and names the expression"                    "$WH/run.log" "wall_hours"
hasnt "…it did not simply run out of sessions"       "$WH/run.log" "reached max_sessions"

# ═══════════════════════════════════════════════════════════════════════════
sec "9g. the git paths the rollback gate does NOT take (eager merge · conflict · recovery)"

# ── eager merge: rollback_on_regression:false → resolve_session commits BEFORE judging ───
EM="$(mkproj eager)"
cat > "$EM/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
n=$(cat .sess 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > .sess
echo "sess-$n" > tracked.txt
git add -A && git commit -qm "worker: session $n"
: > did_work
[ "$n" -ge 2 ] && : > JUDGE_FAIL     # session 2 regresses — but there is NO gate to catch it
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$EM/bin/claude"
cat > "$EM/judges/never.sh" <<'EOF'
#!/bin/sh
echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"keeps the loop alive"}'
EOF
chmod +x "$EM/judges/never.sh"
printf 'goals:\n  - id: worked\n    type: binary\n    judge: { kind: script, cmd: "./judges/check.sh" }\n  - id: endless\n    type: binary\n    judge: { kind: script, cmd: "./judges/never.sh" }\nstop_when: endless\n' > "$EM/goals.yaml"
printf 'project: eager\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nsession_isolation: { enabled: true, rollback_on_regression: false }\n' > "$EM/agg.yaml"
mkrepo "$EM"
agg_do "$EM" run --max-sessions 2 > "$EM/run.log" 2>&1
has   "eager path merges without a post-merge re-test"  "$EM/run.log" "session #1 merged → "
hasnt "…and never takes the rollback-gate wording"      "$EM/run.log" "merged → kept"
hasnt "…so a regressing session is NOT rolled back"     "$EM/run.log" "ROLLED BACK"
is    "…and session 2's regressing work LANDS on base (that is the trade-off)" \
      "$( cd "$EM" && git show HEAD:tracked.txt 2>/dev/null )" "sess-2"

# ── merge conflict: base moved under the session branch ──────────────────────────────────
MC="$(mkproj conflict)"
cat > "$MC/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
BR=$(git rev-parse --abbrev-ref HEAD)
echo branch-side > tracked.txt && git commit -qam "branch edit"
git checkout -q main && echo base-side > tracked.txt && git commit -qam "base moved"
git checkout -q "$BR"          # leave HEAD where agg expects it
: > did_work
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$MC/bin/claude"
printf 'project: conflict\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nsession_isolation: { enabled: true, rollback_on_regression: false }\n' > "$MC/agg.yaml"
mkrepo "$MC"
agg_do "$MC" run --max-sessions 1 > "$MC/run.log" 2>&1
has "a conflicting merge FAILS loudly"              "$MC/run.log" "merge FAILED (conflict)"
has "…and the branch is kept for inspection"        "$MC/run.log" "kept for inspection"
is  "…base is left exactly as it was"               "$( cd "$MC" && git show main:tracked.txt 2>/dev/null )" "base-side"
( cd "$MC" && git rev-parse -q --verify MERGE_HEAD >/dev/null ) \
  && bad "the failed merge left MERGE_HEAD behind" \
  || ok "…and no MERGE_HEAD is stranded (the merge was aborted)"

# ── startup recovery of a merge stranded by an interrupted run ───────────────────────────
# The discriminator is .git/MERGE_MSG (git.rs:79): agg's own merge names the branch_prefix.
# GOTCHA: baseline stop runs BEFORE recovery, so the goal must NOT be met at launch.
strand() { # strand <dir> <branch-name>  → leaves a conflicted, uncommitted merge
  # both sides must differ from mkrepo's committed "base", or `git commit` finds nothing to do
  # and the `&&` chain dies before the merge ever runs.
  ( cd "$1" && git checkout -q -b "$2" && echo branch-side > tracked.txt && git commit -qam b \
     && git checkout -q main && echo main-side > tracked.txt && git commit -qam m \
     && git merge --no-commit "$2" >/dev/null 2>&1; true )
}
SR="$(mkproj recover)"
printf 'project: recover\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nsession_isolation: { enabled: true }\n' > "$SR/agg.yaml"
mkrepo "$SR"; strand "$SR" "agg/recover/session-1"      # name contains the `agg` branch_prefix
( cd "$SR" && git rev-parse -q --verify MERGE_HEAD >/dev/null ) && ok "fixture: a merge is genuinely stranded" || bad "fixture failed to strand a merge"
agg_do "$SR" run --max-sessions 1 > "$SR/run.log" 2>&1
has "agg recovers its OWN stranded merge at startup" "$SR/run.log" "found a leftover staged merge from an interrupted session"
has "…so isolation still turns ON"                   "$SR/run.log" "per-session branch isolation ON"
( cd "$SR" && git rev-parse -q --verify MERGE_HEAD >/dev/null ) \
  && bad "MERGE_HEAD survived recovery" || ok "…and MERGE_HEAD is cleared"

SU="$(mkproj unrelated)"; : > "$SU/NO_WORK"   # baseline must NOT be met, or recovery never runs
printf 'project: unrelated\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nsession_isolation: { enabled: true }\n' > "$SU/agg.yaml"
mkrepo "$SU"; strand "$SU" "hotfix/urgent"              # no `agg` anywhere in the merge message
agg_do "$SU" run --max-sessions 1 > "$SU/run.log" 2>&1
has "a merge agg did NOT start is left alone, with a warning" "$SU/run.log" "WARNING a merge is in progress that agg did not start"
has "…and isolation disables itself rather than trample it"   "$SU/run.log" "running on current branch"
( cd "$SU" && git rev-parse -q --verify MERGE_HEAD >/dev/null ) \
  && ok "…and the user's merge is still there, untouched" || bad "agg destroyed a merge it did not start"

# ═══════════════════════════════════════════════════════════════════════════
sec "9h. memory caps (max_kb on disk · inject_kb per prompt)"
MK="$(mkproj memcap)"; : > "$MK/NO_WORK"
# a worker that leaves a big scratch note → the folded entries blow past a 1 KB cap
cat > "$MK/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
prev=""; for a in "$@"; do [ "$prev" = "-p" ] && printf '%s' "$a" > prompt_latest.txt; prev="$a"; done
sh bin/rec RUN
n=$(cat .sess 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > .sess
mkdir -p .agg/memory
i=0; while [ $i -lt 40 ]; do printf 'padding line %s for session %s\n' "$i" "$n" >> ".agg/memory/session-$n.md"; i=$((i+1)); done
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$MK/bin/claude"
printf 'project: memcap\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nmemory: { enabled: true, max_kb: 1, inject_kb: 1 }\nhooks:\n  on_session_start: ["sh bin/rec INJECT"]\n  on_session_end: ["sh bin/rec GATE"]\n' > "$MK/agg.yaml"
agg_do "$MK" run --max-sessions 4 > "$MK/run.log" 2>&1
exists "AGG_MEMORY.md exists after 4 sessions" "$MK/AGG_MEMORY.md"
SZ=$(wc -c < "$MK/AGG_MEMORY.md" | tr -d ' ')
printf '  AGG_MEMORY.md = %s bytes (cap = 1 KB)\n' "$SZ"
[ "$SZ" -le 1100 ] && ok "…and max_kb=1 caps the durable file (${SZ}B)" \
                   || bad "max_kb not enforced" "${SZ}B > 1 KB"
has "…dropping the OLDEST entries, and saying so" "$MK/AGG_MEMORY.md" "older entries dropped"
has "…the newest session survives the rotation"   "$MK/AGG_MEMORY.md" "session 4"
# inject_kb bounds the per-prompt slice independently of the on-disk file
PB=$(python3 - "$MK/prompt_latest.txt" <<'PY'
import sys
t = open(sys.argv[1]).read()
i = t.find("--- INSTITUTIONAL MEMORY")
print(0 if i < 0 else len(t[i:]))
PY
)
printf '  injected memory block = %s bytes (inject_kb = 1 KB)\n' "$PB"
[ "$PB" -gt 0 ] && ok "the durable slice is INJECTed into the prompt" || bad "no memory block in the prompt"
[ "$PB" -le 2200 ] && ok "…and inject_kb bounds it (${PB}B, incl. the LAST SESSION block)" \
                   || bad "inject_kb not enforced" "${PB}B"

# ═══════════════════════════════════════════════════════════════════════════
sec "9i. the rest of the surface (effort · base_branch · invariants · recheck · flags)"

# ── effort: passed through to the worker as `--effort <value>` (worker.rs:64) ────────────
EF="$(mkproj effort)"
cat > "$EF/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
: > argv.txt; for a in "$@"; do printf '%s\n' "$a" >> argv.txt; done
sh bin/rec RUN; : > did_work
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$EF/bin/claude"
printf 'project: effort\nmodel: fake\neffort: low\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nhooks:\n  on_session_start: ["sh bin/rec INJECT"]\n  on_session_end: ["sh bin/rec GATE"]\n' > "$EF/agg.yaml"
agg_do "$EF" run --max-sessions 2 > "$EF/run.log" 2>&1
has "effort is handed to the worker as --effort" "$EF/argv.txt" "--effort"
has "…with the configured value"                 "$EF/argv.txt" "low"

# ── session_isolation.base_branch: cut sessions from a branch that is NOT the current one ─
BB="$(mkproj basebranch)"
printf 'project: basebranch\nmodel: fake\nresume_prompt: AGG_RESUME.md\nsummary: { enabled: false }\nsession_isolation: { enabled: true, base_branch: "trunk" }\n' > "$BB/agg.yaml"
mkrepo "$BB"
( cd "$BB" && git branch trunk )
agg_do "$BB" run --max-sessions 1 > "$BB/run.log" 2>&1
has "base_branch overrides the launch branch" "$BB/run.log" "base branch 'trunk'"
has "…and sessions are cut off it"            "$BB/run.log" "(off trunk)"

# ── invariants + any_regressed(invariants) ───────────────────────────────────────────────
IV="$(mkproj invariant)"
cat > "$IV/judges/safe.sh" <<'EOF'
#!/bin/sh
if [ -f BREAK ]; then echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"safety broke"}'
else echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"safe"}'; fi
EOF
cat > "$IV/judges/never.sh" <<'EOF'
#!/bin/sh
echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"keeps the loop alive"}'
EOF
chmod +x "$IV/judges/safe.sh" "$IV/judges/never.sh"
cat > "$IV/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN; : > BREAK      # the worker breaks the invariant it was told to preserve
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$IV/bin/claude"
printf 'goals:\n  - id: safe\n    type: binary\n    invariant: true\n    judge: { kind: script, cmd: "./judges/safe.sh" }\n  - id: endless\n    type: binary\n    judge: { kind: script, cmd: "./judges/never.sh" }\nstop_when: endless\nhalt_when: any_regressed(invariants)\n' > "$IV/goals.yaml"
agg_do "$IV" run --max-sessions 3 > "$IV/run.log" 2>&1
is    "a regressed INVARIANT halts the loop (exit 3)"  "$?" "3"
has   "…naming the guard"                              "$IV/run.log" "any_regressed"
hasnt "…and it is not the session cap"                 "$IV/run.log" "reached max_sessions"

# ── recheck: once_met  → the judge LATCHES and is never run again ────────────────────────
OM="$(mkproj oncemet)"
cat > "$OM/judges/counted.sh" <<'EOF'
#!/bin/sh
n=$(cat .judged 2>/dev/null || echo 0); echo $((n+1)) > .judged
echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"done once"}'
EOF
cat > "$OM/judges/never.sh" <<'EOF'
#!/bin/sh
echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"alive"}'
EOF
chmod +x "$OM/judges/counted.sh" "$OM/judges/never.sh"
printf 'goals:\n  - id: latched\n    type: binary\n    recheck: once_met\n    judge: { kind: script, cmd: "./judges/counted.sh" }\n  - id: endless\n    type: binary\n    judge: { kind: script, cmd: "./judges/never.sh" }\nstop_when: endless\n' > "$OM/goals.yaml"
agg_do "$OM" run --max-sessions 2 > "$OM/run.log" 2>&1
is "recheck: once_met judges exactly ONCE, then latches (baseline only)" \
   "$(cat "$OM/.judged" 2>/dev/null)" "1"

# ── recheck: on_change → re-judged only when a declared input changes ────────────────────
OC="$(mkproj onchange)"
cat > "$OC/judges/counted.sh" <<'EOF'
#!/bin/sh
n=$(cat .judged 2>/dev/null || echo 0); echo $((n+1)) > .judged
echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'
EOF
chmod +x "$OC/judges/counted.sh"
echo original > "$OC/watched.txt"
cat > "$OC/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
n=$(cat .sess 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > .sess
[ "$n" = "2" ] && echo changed > watched.txt   # only session 2 touches the watched input
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$OC/bin/claude"
printf 'goals:\n  - id: gated\n    type: binary\n    recheck: on_change\n    recheck_inputs: ["watched.txt"]\n    judge: { kind: script, cmd: "./judges/counted.sh" }\nstop_when: gated\n' > "$OC/goals.yaml"
agg_do "$OC" run --max-sessions 2 > "$OC/run.log" 2>&1
is "recheck: on_change re-judges only when the input changed (baseline + session 2)" \
   "$(cat "$OC/.judged" 2>/dev/null)" "2"

# ── validate_recheck rejects a latched invariant (it could never see its own regression) ──
BAD="$(mkproj badrecheck)"
printf 'goals:\n  - id: safe\n    type: binary\n    invariant: true\n    recheck: once_met\n    judge: { kind: script, cmd: "./judges/check.sh" }\nstop_when: safe\n' > "$BAD/goals.yaml"
agg_do "$BAD" run --max-sessions 1 > "$BAD/run.log" 2>&1
[ $? -ne 0 ] && ok "an invariant with recheck: once_met is REJECTED, not silently latched" \
             || bad "a latched invariant was accepted"
has "…with an actionable message" "$BAD/run.log" "invariants must"

# ── a hanging judge is killed by its timeout; the loop survives (judges are crash-safe) ──
JT="$(mkproj judgetimeout)"; : > "$JT/NO_WORK"
cat > "$JT/judges/slow.sh" <<'EOF'
#!/bin/sh
sleep 30
echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"never gets here"}'
EOF
chmod +x "$JT/judges/slow.sh"
printf 'goals:\n  - id: slow\n    type: binary\n    judge: { kind: script, cmd: "./judges/slow.sh", timeout: 1 }\nstop_when: slow\n' > "$JT/goals.yaml"
JTS=$(date +%s)
agg_do "$JT" run --max-sessions 1 > "$JT/run.log" 2>&1
is "a judge that hangs does not hang the loop (exit 4, the cap)" "$?" "4"
[ $(( $(date +%s) - JTS )) -lt 25 ] && ok "…the judge timeout fired instead of waiting it out" \
                                    || bad "the judge ran to completion; timeout ignored"
hasnt "…and the hung judge never reports met" "$JT/run.log" "never gets here"

# ── AGG_MEMORY_MAX_KB env override (config.rs:324) ───────────────────────────────────────
EV="$(mkproj memenv)"; : > "$EV/NO_WORK"
cat > "$EV/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
n=$(cat .sess 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > .sess
mkdir -p .agg/memory
i=0; while [ $i -lt 200 ]; do printf 'padding line %s of session %s\n' "$i" "$n" >> ".agg/memory/session-$n.md"; i=$((i+1)); done
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$EV/bin/claude"
# CONTROL first: without the override the file must exceed 1 KB, or the assertion below is
# vacuous (it would "pass" simply because nothing was ever big enough to cap).
( cd "$EV" && PATH="$EV/bin:$PATH" "$AGG" run --max-sessions 4 > uncapped.log 2>&1 )
RAW=$(wc -c < "$EV/AGG_MEMORY.md" 2>/dev/null | tr -d ' ')
[ "${RAW:-0}" -gt 1100 ] && ok "control: uncapped memory really does exceed 1 KB (${RAW}B)" \
                         || bad "control failed — the memcap assertion would be vacuous" "${RAW}B"
rm -f "$EV/AGG_MEMORY.md" "$EV/.sess"; rm -rf "$EV/.agg"
( cd "$EV" && PATH="$EV/bin:$PATH" AGG_MEMORY_MAX_KB=1 "$AGG" run --max-sessions 4 > run.log 2>&1 )
SZ=$(wc -c < "$EV/AGG_MEMORY.md" 2>/dev/null | tr -d ' ')
[ "${SZ:-99999}" -le 1100 ] && ok "AGG_MEMORY_MAX_KB=1 overrides the config default (${RAW}B → ${SZ}B)" \
                            || bad "the env override was ignored" "${SZ}B"
has "…and the rotation notice proves the cap actually fired" "$EV/AGG_MEMORY.md" "older entries dropped"

# ── global flags: --dir, --config, --goals ───────────────────────────────────────────────
GF="$(mkproj globalflags)"
( cd "$WS" && PATH="$GF/bin:$PATH" "$AGG" --dir "$GF" run --max-sessions 2 > "$GF/dirrun.log" 2>&1 )
is  "--dir runs the loop in another directory (exit 0)" "$?" "0"
exists "…and the worker really worked there"           "$GF/did_work"

GC="$(mkproj cfgflags)"
mv "$GC/agg.yaml" "$GC/custom.yaml"; mv "$GC/goals.yaml" "$GC/custom-goals.yaml"
agg_do "$GC" --config "$GC/custom.yaml" --goals "$GC/custom-goals.yaml" run --max-sessions 2 > "$GC/run.log" 2>&1
is  "--config/--goals accept non-default filenames (exit 0)" "$?" "0"
has "…and the run really reached its goal"                   "$GC/run.log" "STOP condition satisfied"

# ── `agg send …` subcommands (the aliases the web UI mirrors) ────────────────────────────
SN="$(mkproj sendcmds)"; : > "$SN/NO_WORK"; echo 2 > "$SN/WORKER_SLEEP"
agg_bg SNL "$SN" run.log run --max-sessions 8
waitfor 30 "live loop for the send-alias tests" grep -q "RUN=run" "$SN/trace.txt"
agg_do "$SN" send pause > /dev/null 2>&1
waitfor 30 "agg send pause parks the loop" grep -q "pause → waiting" "$SN/run.log"
agg_do "$SN" send resume > /dev/null 2>&1
waitfor 30 "agg send resume continues it" grep -q "resume → continuing" "$SN/run.log"
agg_do "$SN" send budget 999999 > /dev/null 2>&1
waitfor 30 "agg send budget is applied" grep -q "set-budget" "$SN/run.log"
agg_do "$SN" send stop "via send" > /dev/null 2>&1
waitfor 40 "agg send stop ends the loop" bash -c "! kill -0 $SNL 2>/dev/null"
wait $SNL; is "…exit 0" "$?" "0"
is "…with the reason that send stop gave" "$(finish_reason "$SN")" "stopped via bus: via send"

# ═══════════════════════════════════════════════════════════════════════════
sec "9j. the docs describe the tool that actually exists"
# A hand-written CLI table rots the moment a subcommand is added. Assert every clap subcommand
# appears in the README, and that every relative link in the README resolves to a real file.
"$AGG" --help > "$WS/help.txt" 2>&1
python3 - "$ROOT" "$WS/help.txt" <<'PY'
import re, sys, pathlib
root = pathlib.Path(sys.argv[1]); helptxt = open(sys.argv[2]).read()
readme = (root / "README.md").read_text()

# clap prints subcommands one-per-line under "Commands:"
block = helptxt.split("Commands:", 1)[1].split("Options:", 1)[0]
cmds = {m.group(1) for m in re.finditer(r"^\s{2}(\w[\w-]*)\s{2,}", block, re.M)} - {"help"}
missing = sorted(c for c in cmds if f"agg {c}" not in readme)
print(f"  subcommands in --help: {len(cmds)}; missing from README: {missing or 'none'}")
sys.exit(1 if missing else 0)
PY
[ $? -eq 0 ] && ok "every CLI subcommand is documented in the README" \
             || bad "the README's CLI table is missing a subcommand"

python3 - "$ROOT" <<'PY'
import re, sys, pathlib
root = pathlib.Path(sys.argv[1])
readme = (root / "README.md").read_text()
links = re.findall(r"\]\(([^)#:]+)\)", readme) + re.findall(r'src="([^"]+)"', readme)
broken = [l for l in links if not l.startswith(("http", "#")) and not (root / l).exists()]
print(f"  relative links checked: {len(links)}; broken: {broken or 'none'}")
sys.exit(1 if broken else 0)
PY
[ $? -eq 0 ] && ok "every relative link/image in the README resolves" \
             || bad "the README has a broken relative link"

exists "the loop diagram is committed"        "$ROOT/assets/loop.png"
exists "the config reference exists"          "$ROOT/docs/CONFIG.md"
exists "the hello-agg example exists"         "$ROOT/examples/hello-agg/README.md"
exists "the p-vs-np example exists"           "$ROOT/examples/p-vs-np/README.md"
hasnt  "…and the retired flowchart is gone"   "$ROOT/README.md" "how-it-works"

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
  T="$(mkproj tuidemo)"
  # the Activity pane must OVERFLOW, or there is nothing to scroll and follow-mode is moot:
  # emit ~60 `assistant` text events so max_scroll > 0.
  cat > "$T/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
i=1; while [ $i -le 60 ]; do
  printf '{"type":"assistant","message":{"content":[{"type":"text","text":"thinking step %s"}]}}\n' "$i"
  i=$((i+1))
done
: > did_work
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
  chmod +x "$T/bin/claude"
  agg_do "$T" run --max-sessions 1 > "$T/run.log" 2>&1

  # drive <name> <key-script> <timeout>  → runs the TUI, leaves the de-ANSI'd frames in $T/<name>.txt
  drive() { ( cd "$T" && python3 "$DRIVE" --seq "$2" --timeout "$3" -- "$AGG" dashboard > "$1.raw" 2>&1 ); local r=$?; deansi "$T/$1.raw" "$T/$1.txt"; return $r; }

  # NOTE on grepping a pty capture: ratatui re-emits only CHANGED cells. Going from `[⏵live]` to
  # `[paused]` leaves the shared `e` untouched, so the stream holds `paus`…`d` and the word never
  # appears contiguously. `RESIZE` (a pseudo-key in tui_drive.py) resizes the pty, which makes
  # ratatui repaint in FULL — put it after the keys under test and before `q`.
  drive base "2.0:q" 25
  is "TUI launches on a pty and quits on 'q'" "$?" "0"
  has "…paints the project name"    "$T/base.txt" "tuidemo"
  has "…paints the goal"            "$T/base.txt" "worked"
  has "…paints the phase field"     "$T/base.txt" "phase"
  has "…paints the worker's activity stream" "$T/base.txt" "thinking step"
  has "…paints the finished banner" "$T/base.txt" "FINISHED"
  has "…paints the keybinding help" "$T/base.txt" "q=quit"
  # focus starts on Activity, follow-mode starts on
  has "…Activity starts focused (▸)"        "$T/base.txt" "▸ Activity"
  has "…and auto-follow starts on (⏵live)"  "$T/base.txt" "⏵live"
  hasnt "…Goals is not focused to begin with" "$T/base.txt" "▸ Goals"
  hasnt "…and follow is not paused"           "$T/base.txt" "paused"

  # a user presses Tab to move focus, f to toggle follow, arrows to scroll
  drive tab "1.5:Tab,0.6:RESIZE,0.8:q" 25
  is  "Tab is accepted and the TUI still quits" "$?" "0"
  has "…Tab moves focus to Goals (▸ Goals)"     "$T/tab.txt" "▸ Goals"

  # `f` at the bottom used to be a no-op: draw_activity re-pinned anything at max_scroll,
  # so the pause was undone by the very next repaint.
  drive follow "1.5:f,0.6:RESIZE,0.8:q" 25
  is  "f is accepted" "$?" "0"
  has "…f pauses auto-follow, and the pause survives the repaint" "$T/follow.txt" "Activity  [paused]"

  drive refollow "1.5:f,0.4:f,0.6:RESIZE,0.8:q" 25
  is    "…and f again resumes it" "$?" "0"
  has   "…back to ⏵live"          "$T/refollow.txt" "Activity  [⏵live]"
  hasnt "…and not left paused"    "$T/refollow.txt" "Activity  [paused]"

  drive up "1.5:Up,0.6:RESIZE,0.8:q" 25
  is  "Up leaves follow-mode" "$?" "0"
  has "…and the pane reads paused" "$T/up.txt" "Activity  [paused]"

  drive gG "1.5:g,0.4:G,0.6:RESIZE,0.8:q" 25
  is  "g jumps to the oldest event, G re-pins to the newest" "$?" "0"
  has "…and G restores ⏵live" "$T/gG.txt" "Activity  [⏵live]"

  drive scroll "1.5:Down,0.2:Down,0.2:Up,0.2:PageDown,0.2:G,0.2:g,0.6:q" 25
  is "arrows/PageDown/g/G scroll without crashing, and 'q' still quits" "$?" "0"

  drive esc "1.5:Esc" 25
  is "Esc quits too" "$?" "0"

  drive ignore "1.0:x" 8
  is "…and an unbound key does NOT quit" "$?" "124"

  # a LIVE loop must render one of the four stage names, not the old vocabulary
  TL="$(mkproj tuilive)"; : > "$TL/NO_WORK"; echo 5 > "$TL/WORKER_SLEEP"
  agg_bg TLP "$TL" run.log run --max-sessions 3
  waitfor 30 "live loop for the TUI" grep -q "RUN=run" "$TL/trace.txt"
  ( cd "$TL" && python3 "$DRIVE" --seq "2.0:q" --timeout 25 -- "$AGG" dashboard > tui.raw 2>&1 )
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

  # ── REAL BROWSER: Chromium clicks the actual buttons ─────────────────────────────────
  # Everything above only proves the BFF proxies JSON. This drives the DOM the way a user
  # does — Pause / Resume / Inject… / Budget… / Stop, including the confirm() dialog — and
  # then checks the effect landed in the real loop's log and the next worker's prompt.
  if ! python3 -c "import playwright" 2>/dev/null; then
    skip "browser click-through" "pip install playwright && playwright install chromium"
  else
    python3 "$ROOT/scripts/web_e2e.py" --url "http://127.0.0.1:$WPORT" --project "$W" \
            --shots "$W/shots" > "$W/browser.log" 2>&1
    BRC=$?
    sed -n 's/^  /  /p' "$W/browser.log" | grep -E '✔|✘' || true
    BP=$(grep -c '✔' "$W/browser.log" || true); BF=$(grep -c '✘' "$W/browser.log" || true)
    PASS=$((PASS + BP)); FAIL=$((FAIL + BF))
    if [ "$BF" -gt 0 ]; then
      while IFS= read -r l; do FAILED+=("browser: $l"); done < <(grep -oE '•.*' "$W/browser.log" | sed 's/^• //')
    fi
    [ "$BRC" = "0" ] || [ "$BF" -gt 0 ] || bad "browser click-through crashed" "$(tail -3 "$W/browser.log")"
    exists "…screenshots captured for inspection" "$W/shots/01-live.png"
  fi

  # the browser test ends by clicking Stop, so the loop must be gone
  waitfor 40 "…the loop really stopped after the browser clicked ⏹ Stop" bash -c "! kill -0 $WLOOP 2>/dev/null"
  wait $WLOOP; is "…exit 0" "$?" "0"
  is "…with the reason the browser sent" "$(finish_reason "$W")" "stopped via bus: stopped from web"

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
