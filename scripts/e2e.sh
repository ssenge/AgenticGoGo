#!/usr/bin/env bash
# Full end-to-end acceptance suite, from a USER's perspective.
#
# Drives the real `agg` binary, the real TUI, the real `agg serve` HTTP API and the real
# SvelteKit web app — no mocks except the `claude` worker itself (a shell stub on PATH that
# emits valid stream-json), because a real model is non-deterministic, costs money and needs
# network. Everything else is the shipping code.
#
# The model this suite drives (§4/§5/§7 of internal/SEQUENCES.md):
#   - a judge IS a goal, resolved by NAME from agg/judges/<name>.{sh,md}. There is no goals.yaml.
#   - one config file, agg/agg.yaml (defaults / judge / steps / sequence). done_if / abort_if.
#   - session isolation is MANDATORY: every `agg run` needs a git repo, a clean tree, a born HEAD,
#     and stages + gates every session — so the fixtures git-init and the workers COMMIT their work
#     (uncommitted work resolves as NoChanges and the gate restores base, so a met verdict on it
#     never counts).
#   - all runtime state lives under agg/state/ (gitignored).
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
snap()          { python3 -c "import json;print(json.load(open('$1/agg/state/state.json'))['$2'])" 2>/dev/null; }
phase_of()      { snap "$1" phase; }
finish_reason() { snap "$1" finish_reason; }

# strip ANSI/CR so a TUI capture can be grepped
deansi() { LC_ALL=C sed -e $'s/\x1b\[[0-9;?]*[A-Za-z]//g' -e $'s/\x1b[()][AB0]//g' -e 's/\r//g' "$1" > "$2"; }

# ---------------------------------------------------------------------------
# fixture: a project with a fake `claude` on PATH, in a CLEAN git repo (session isolation is
# mandatory — an `agg run` refuses to start without a repo + clean tree + born HEAD).
#   the worker records the live phase, dumps the prompt it was handed, honours
#   WORKER_SLEEP / NO_WORK / WORKER_TOKENS / WORKER_COST toggle-files, and — unless NO_WORK —
#   COMMITS its `did_work` marker on the session branch (uncommitted work is NoChanges and the
#   gate would restore base, so its met verdict would never count).
#   the judge `worked` records the live phase and honours JUDGE_FAIL.
# git_init leaves ONE empty commit, so every fixture file the caller writes stays UNTRACKED —
# which `is_clean` ignores, so the tree reads clean and sessions branch from a born `main`.
# ---------------------------------------------------------------------------
mkproj() { # mkproj <name>
  local name=$1
  local d="$WS/$name"
  mkdir -p "$d/bin" "$d/agg/judges"

  cat > "$d/bin/rec" <<'EOF'
#!/bin/sh
printf '%s=%s\n' "$1" "$(sed -n 's/.*"phase":"\([a-z]*\)".*/\1/p' agg/state/state.json)" >> trace.txt
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
if [ ! -f NO_WORK ]; then
  : > did_work
  git add did_work >/dev/null 2>&1
  git commit -qm "worker: did_work" >/dev/null 2>&1
fi
tok=1;   [ -f WORKER_TOKENS ] && tok=$(cat WORKER_TOKENS)
cost=0;  [ -f WORKER_COST ]   && cost=$(cat WORKER_COST)
printf '{"type":"result","subtype":"success","is_error":false,"result":"done","usage":{"output_tokens":%s},"total_cost_usd":%s}\n' "$tok" "$cost"
exit 0
EOF

  cat > "$d/agg/judges/worked.sh" <<'EOF'
#!/bin/sh
sh bin/rec VERIFY
if [ -f JUDGE_FAIL ] || [ ! -f did_work ]; then
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'
else
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"did_work present"}'
fi
EOF

  chmod +x "$d/bin/rec" "$d/bin/claude" "$d/agg/judges/worked.sh"

  cat > "$d/agg/agg.yaml" <<EOF
project: $name
defaults: { model: fake }
steps:
  worker: {}
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
hooks:
  on_session_start: ["sh bin/rec INJECT"]
  on_session_end: ["sh bin/rec GATE"]
EOF
  echo "create the file did_work" > "$d/agg/AGG_STATE.md"

  ( cd "$d" && git init -q -b main && git config user.email t@t && git config user.name t \
    && git commit -q --allow-empty -m init )
  echo "$d"
}

# commit whatever the fixture has written so far onto `main` (for the git-isolation sections that
# need a TRACKED base file, e.g. tracked.txt, so a worker's edit-and-commit can merge/roll back).
gitbase() { ( cd "$1" && git add -A >/dev/null 2>&1 && git commit -qm base >/dev/null 2>&1; true ); }

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
# 1-4, 6. scaffolding · exit codes · stage trace · status/history/dashboard · interrupt
#
# DELETED — these are covered by `cargo test --test cli` (tests/cli.rs), which drives the
# same paths against the same fake-claude shim, in-process and in CI. Kept here only as a
# map so a future reader does not think the coverage vanished:
#
#   init/plan/judge/doctor      → init_then_plan_shows_scoreboard · doctor_passes_a_good_setup
#                                 doctor_flags_a_broken_setup · judge_runs_one_name_and_prints_raw_verdict
#                                 config_lives_in_the_agg_folder
#   exit codes 0/3/4            → run_drives_a_correction_loop_to_stop · run_without_config_gives_actionable_hint
#                                 dollar_budget_aborts_the_loop · max_sessions_cap_exits_4_and_says_so
#                                 run_stops_immediately_when_goal_already_met
#   four-stage trace            → phase_names_the_four_outer_loop_stages
#   baseline enters no stage    → a_baseline_satisfied_run_enters_no_stage
#   status/history/dashboard    → status_and_history_json_are_machine_readable
#   interrupt (Ctrl-C)          → interrupt_during_run_skips_verify_and_the_exit_log
#   the rollback GATE (moat)    → rollback_gate_unlands_a_regressing_merge ·
#                                 rollback_gate_keeps_merge_when_a_judge_merely_flakes ·
#                                 a_broken_judge_does_not_abort_the_run_wearing_a_regressions_clothes
#   the new step/sequence model → an_unknown_step_in_the_sequence_is_a_startup_error ·
#                                 a_sequence_of_only_skip_judges_is_refused_at_startup ·
#                                 a_skip_judges_span_is_gated_and_merged_by_the_next_judged_step ·
#                                 done_if_all_goals_ignores_an_if_condition_judge
#
# What REMAINS in this file is what cli.rs cannot or does not drive: live loops steered over
# the bus, detached runs, spawn, the watchdog, git merge/conflict/recovery paths, the HTTP API,
# a real pty TUI, the browser, and the observable end-to-end shape of the sequence grammar.
sec "5. steering a LIVE loop over the bus  (inject · note · pause/resume · budget · stop)"

# a queued command with no loop running is accepted but warns
Q="$(mkproj prearm)"
agg_do "$Q" send inject "pre-armed" > "$Q/q.log" 2>&1
is  "agg send inject with no loop exits 0 (pre-arming is legal)" "$?" "0"
has "…but warns that no loop is running" "$Q/q.log" "NO loop is running"

# --- a slow loop we can steer while it runs
S="$(mkproj steer)"; : > "$S/NO_WORK"; echo 2 > "$S/WORKER_SLEEP"
agg_bg LOOP "$S" run.log run --max-sessions 6
waitfor 30 "live loop reaches its first RUN" grep -q "RUN=run" "$S/trace.txt"

agg_do "$S" send note "hello-bus" > /dev/null 2>&1
agg_do "$S" send inject "OPERATOR_MARKER_XYZ" > /dev/null 2>&1
waitfor 30 "injected instruction reaches the NEXT worker prompt" grep -q "OPERATOR_MARKER_XYZ" "$S/prompt_latest.txt"
has "…as a HIGH-PRIORITY header"     "$S/prompt_latest.txt" "HIGH-PRIORITY OPERATOR INSTRUCTION"
has "…and the AGG_STATE.md survives"  "$S/prompt_latest.txt" "create the file did_work"
has "agg send note is logged by the loop" "$S/run.log" "[bus] note: hello-bus"

agg_do "$S" send pause > /dev/null 2>&1
waitfor 30 "agg send pause parks the loop in INJECT" grep -q "pause → waiting for resume/stop" "$S/run.log"
is  "…and the published phase says inject" "$(phase_of "$S")" "inject"
agg_do "$S" send resume > /dev/null 2>&1
waitfor 30 "agg send resume continues the loop" grep -q "resume → continuing" "$S/run.log"

agg_do "$S" stop "e2e-stop-reason" > /dev/null 2>&1
waitfor 40 "agg stop ends the loop" bash -c "! kill -0 $LOOP 2>/dev/null"
wait $LOOP; RC=$?
is  "…exit 0 (an operator stop is a clean end)" "$RC" "0"
# the reason is LOGGED as `[bus] stop → …` and STORED as dash.finish_reason (state.json);
# "stopped via bus: …" is never printed to the log.
has "…the loop logs the bus stop"      "$S/run.log" "[bus] stop → e2e-stop-reason"
is  "…and records the finish reason"   "$(finish_reason "$S")" "stopped via bus: e2e-stop-reason"
absent "…run.pid cleared by the Drop guard" "$S/agg/state/run.pid"
is  "…ledger finalized as stopped" \
    "$(python3 -c "import json;print(json.load(open('$S/agg/state/project.json'))['runs'][-1]['end_reason'])")" "stopped"

# --- budget steering halts a live loop (budget + abort_if live under `sequence:` now, §4.1)
B="$(mkproj budget)"; : > "$B/NO_WORK"; echo 2 > "$B/WORKER_SLEEP"; echo 500 > "$B/WORKER_TOKENS"
cat > "$B/agg/agg.yaml" <<'EOF'
project: budget
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
  abort_if: "over_budget"
summary: { enabled: false }
EOF
agg_bg BLOOP "$B" run.log run --max-sessions 6
waitfor 30 "live loop for budget test reaches RUN" grep -q "RUN=run" "$B/trace.txt"
agg_do "$B" send budget 1 > /dev/null 2>&1
waitfor 40 "agg send budget <n> halts the running loop" bash -c "! kill -0 $BLOOP 2>/dev/null"
wait $BLOOP; RC=$?
is  "…exit 3 (a guard fired)" "$RC" "3"
has "…names over_budget"      "$B/run.log" "over_budget"

# ═══════════════════════════════════════════════════════════════════════════
sec "7. detached run + agg stop"
DT="$(mkproj detach)"; : > "$DT/NO_WORK"; echo 2 > "$DT/WORKER_SLEEP"
agg_do "$DT" run --detach --max-sessions 6 > "$DT/detach.log" 2>&1
is  "agg run --detach returns immediately (exit 0)" "$?" "0"
waitfor 30 "…writes agg/state/run.pid" test -f "$DT/agg/state/run.pid"
exists "…and logs to agg/state/run.log" "$DT/agg/state/run.log"
waitfor 30 "…the detached loop really runs" grep -q "RUN=run" "$DT/trace.txt"
agg_do "$DT" run --max-sessions 1 > "$DT/second.log" 2>&1
[ $? -ne 0 ] && ok "double-run guard refuses a second loop" || bad "a second concurrent loop was allowed"
has "…and says which pid holds it" "$DT/second.log" "already running"
agg_do "$DT" stop "detached-stop" > /dev/null 2>&1
waitfor 40 "agg stop ends the detached loop" bash -c "! test -f '$DT/agg/state/run.pid'"
ok "…run.pid cleared after the detached loop exits"

# ═══════════════════════════════════════════════════════════════════════════
sec "8. agg spawn — long tasks that outlive a session"
SP="$(mkproj spawn)"
agg_do "$SP" spawn --name e2e-task --reason "long sim" -- sleep 20 > "$SP/spawn.log" 2>&1
is  "agg spawn exits 0" "$?" "0"
exists "…registers the task in agg/state/spawns.json" "$SP/agg/state/spawns.json"
python3 -c "
import json;d=json.load(open('$SP/agg/state/spawns.json'))
e=[x for x in d['spawns'] if x['name']=='e2e-task']
assert e, 'task not registered'
assert e[0]['status']=='running', e[0]['status']
assert 'long sim' in e[0]['reason']" && ok "…status=running with the operator's reason" || bad "spawns.json malformed"
agg_do "$SP" run --max-sessions 1 > "$SP/run.log" 2>&1
has "…the next session's prompt is told about it" "$SP/prompt_latest.txt" "e2e-task"
has "…including WHY, so it polls instead of relaunching" "$SP/prompt_latest.txt" "long sim"
# kill exactly the pid we registered — never a blanket pkill of the user's processes
SPID=$(python3 -c "import json;print([x for x in json.load(open('$SP/agg/state/spawns.json'))['spawns'] if x['name']=='e2e-task'][0]['pid'])" 2>/dev/null || true)
[ -n "${SPID:-}" ] && kill -9 "$SPID" 2>/dev/null || true

# ═══════════════════════════════════════════════════════════════════════════
sec "9. institutional memory — carried ACROSS separate \`agg run\` invocations"
# The single-run memory contract (AGG_MEMORY.md is written · exactly ONE entry per completed
# session, the early fold superseded · the scratch note is deleted after folding) is covered by
# tests/cli.rs — institutional_memory_is_written_without_worker_cooperation and
# worker_written_memory_note_is_folded. What stays HERE is the part cli.rs does not drive:
# memory surviving across TWO separate `agg run` PROCESSES and reaching the next one's prompt.

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

GI="$(mkproj iso)"
cat > "$GI/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
echo worker-edit > tracked.txt
git add tracked.txt && git commit -qm "worker: session work"   # the worker commits on its branch
: > did_work
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$GI/bin/claude"
cat > "$GI/agg/agg.yaml" <<'EOF'
project: iso
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
memory: { enabled: false }
EOF
echo base > "$GI/tracked.txt"
gitbase "$GI"
agg_do "$GI" run --max-sessions 1 > "$GI/run.log" 2>&1
has "isolation cuts a per-session branch off the base"  "$GI/run.log" "[iso] session #1 on branch"
has "…and a green session is MERGED and KEPT"           "$GI/run.log" "merged → kept"
is  "…so the worker's commit is on base" \
    "$( cd "$GI" && git show HEAD:tracked.txt 2>/dev/null )" "worker-edit"

# now regress a previously-met judge → the GATE must roll the merge back.
# `worked` is an INVARIANT (in the DoD-set, which the regression gate scopes to); a second,
# never-met `endless` judge is the done_if that keeps the loop alive past session 1.
GR="$(mkproj rollback)"
cat > "$GR/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
n=$(cat .sess 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > .sess
echo "sess-$n" > tracked.txt
git add tracked.txt && git commit -qm "worker: session $n"
: > did_work
[ "$n" -ge 2 ] && : > JUDGE_FAIL   # session 2 REGRESSES the goal session 1 had met
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$GR/bin/claude"
cat > "$GR/agg/judges/endless.sh" <<'EOF'
#!/bin/sh
echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"keeps the loop alive"}'
EOF
chmod +x "$GR/agg/judges/endless.sh"
cat > "$GR/agg/agg.yaml" <<'EOF'
project: rollback
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "endless"
  invariants: [worked]
summary: { enabled: false }
EOF
echo base > "$GR/tracked.txt"
gitbase "$GR"
agg_do "$GR" run --max-sessions 2 > "$GR/run.log" 2>&1
has "session 1 (green) is merged onto base"          "$GR/run.log" "session #1 merged → kept"
has "session 2 (regressing) is ROLLED BACK"          "$GR/run.log" "session #2 ROLLED BACK"
is  "…and its work NEVER lands on base (base still holds session 1)" \
    "$( cd "$GR" && git show HEAD:tracked.txt 2>/dev/null )" "sess-1"
has "…the durable memory says the work is NOT on base" "$GR/agg/state/AGG_MEMORY.md" "NOT on the base branch"
has "…and the session branch is kept for inspection"   "$GR/run.log" "kept for inspection"

# the worker's own veto: writing the red file discards the session, merged or not
GV="$(mkproj veto)"
cat > "$GV/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
echo vetoed-work > tracked.txt
git add tracked.txt && git commit -qm "worker: work I do not trust"
: > AGG_RED            # the worker vetoes its own session
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$GV/bin/claude"
cat > "$GV/agg/agg.yaml" <<'EOF'
project: veto
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
session_isolation: { red_file: "AGG_RED" }
EOF
echo base > "$GV/tracked.txt"
gitbase "$GV"
agg_do "$GV" run --max-sessions 1 > "$GV/run.log" 2>&1
has "a worker that writes the red file VETOES its own session" "$GV/run.log" "VETOED"
is  "…and none of its work reaches base" \
    "$( cd "$GV" && git show HEAD:tracked.txt 2>/dev/null )" "base"

# ═══════════════════════════════════════════════════════════════════════════
sec "9c. worker-failure paths (rate-limit backoff · dud-worker abort · hung-worker watchdog)"
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
cat > "$RL/agg/agg.yaml" <<'EOF'
project: ratelimit
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
ratelimit_backoff_secs: 1
hooks:
  on_session_start: ["sh bin/rec INJECT"]
  on_session_end: ["sh bin/rec GATE"]
EOF
agg_do "$RL" run --max-sessions 2 > "$RL/run.log" 2>&1
has "a rate-limited session backs off"        "$RL/run.log" "rate limit detected"
has "…and is flagged on the exit line"        "$RL/run.log" "[RATE-LIMITED]"
hasnt "…and is NEVER judged"                  "$RL/run.log" "running judges…"
absent "…and leaves NO durable memory entry"  "$RL/agg/state/AGG_MEMORY.md"
is "…the trace shows no VERIFY/GATE after RUN" \
   "$(tr '\n' ' ' < "$RL/trace.txt")" "VERIFY=verify INJECT=inject RUN=run INJECT=inject RUN=run "

# A worker that cannot even START (bad model, dead auth, unknown worker_arg) exits non-zero having
# produced ZERO tokens. The loop used to just go round again: spawn, die, judge, spawn, die, judge
# — looking exactly like a healthy autonomous run, printing a scoreboard every cycle, doing nothing
# whatsoever, forever (--max-sessions defaults to unlimited). This is how Copilot's `model: auto` +
# `effort: max` default shipped broken and looked fine. NOTE the stub emits no rate-limit text, so
# this must NOT be confused with the backoff path above — which also exits 1 with 0 tokens, and
# which must still be allowed to retry (the section above is what proves the exemption holds).
DUD="$(mkproj dud)"
cat > "$DUD/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
echo "Error: unknown model" >&2
exit 1
EOF
chmod +x "$DUD/bin/claude"
# NO --max-sessions: this is the unbounded case that used to spin forever.
agg_do "$DUD" run > "$DUD/run.log" 2>&1
is  "a worker that never starts ABORTS the run (exit 1)" "$?" "1"
has "…after a bounded number of tries, not forever"      "$DUD/run.log" "failed to start 3 times in a row"
has "…saying it never reached the model"                 "$DUD/run.log" "ZERO tokens"
has "…and how to diagnose it"                            "$DUD/run.log" "agg doctor"

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
cat > "$WD/agg/agg.yaml" <<'EOF'
project: watchdog
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
watchdog: { idle_secs: 3, cpu_grace: 2 }
EOF
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
sec "9d. prompt composition (prompt_includes) and lifecycle hooks"
# resume_sessions is GONE (§4.1/§7.3): a per-agent session id cannot cross a mixed sequence, so the
# key is refused at parse time — there is no `--resume` plumbing left to assert.
PI="$(mkproj promptinc)"; : > "$PI/NO_WORK"
echo "TOOLING_FRAGMENT_ZZZ" > "$PI/frag.md"
cat > "$PI/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
prev=""; for a in "$@"; do
  [ "$prev" = "-p" ] && printf '%s' "$a" > prompt_latest.txt
  prev="$a"
done
sh bin/rec RUN
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$PI/bin/claude"
cat > "$PI/agg/agg.yaml" <<'EOF'
project: promptinc
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
prompt_includes: ["frag.md"]
hooks:
  on_start: ["echo HOOK_ON_START"]
  on_stop: ["echo HOOK_ON_STOP"]
  background: ["sleep 30"]
  on_session_start: ["sh bin/rec INJECT"]
  on_session_end: ["sh bin/rec GATE"]
EOF
agg_do "$PI" run --max-sessions 2 > "$PI/run.log" 2>&1
has "prompt_includes are prepended to every prompt" "$PI/prompt_latest.txt" "TOOLING_FRAGMENT_ZZZ"
has "…above the AGG_STATE.md standing instructions"  "$PI/prompt_latest.txt" "create the file did_work"
has "on_start hook runs once at launch"             "$PI/run.log" "HOOK_ON_START"
has "on_stop hook runs on exit (Drop guard)"        "$PI/run.log" "HOOK_ON_STOP"
has "background hook is spawned"                    "$PI/run.log" "[hook:background]"

# ═══════════════════════════════════════════════════════════════════════════
sec "9e. LLM-backed pieces (llm judge · summarizer) against a stubbed model"
# A judge with a `.md` extension is an LLM judge (§5.1): the rubric IS the file, inputs come from
# its own frontmatter. It runs on the RULER (`judge:` block), as does the summarizer.
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
git add did_work >/dev/null 2>&1
git commit -qm "worker: did_work" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$LJ/bin/claude"
cat > "$LJ/agg/judges/reviewed.md" <<'EOF'
---
inputs: []
---
Decide whether the work is done. Output ONLY the verdict JSON on the last line.
EOF
rm -f "$LJ/agg/judges/worked.sh"   # this project's DoD is the LLM judge, not the script judge
cat > "$LJ/agg/agg.yaml" <<'EOF'
project: llmjudge
defaults: { model: fake }
judge: { agent: claude, model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "reviewed"
summary: { enabled: true, min_interval_secs: 0 }
EOF
agg_do "$LJ" run --max-sessions 2 > "$LJ/run.log" 2>&1
is  "an llm judge drives the loop to its stop condition (exit 0)" "$?" "0"
has "…it reports not-met at baseline"          "$LJ/run.log" "LLM_JUDGE_SAYS_NOT_YET"
has "…then met after the worker ran"           "$LJ/run.log" "LLM_JUDGE_SAYS_OK"
has "the summarizer runs and logs a cumulative summary" "$LJ/run.log" "CUMULATIVE_SUMMARY_X"
has "…and a windowed summary"                           "$LJ/run.log" "WINDOWED_SUMMARY_Y"
is  "…and the summary is published to state.json" "$(snap "$LJ" summary_cumulative)" "CUMULATIVE_SUMMARY_X"

# ═══════════════════════════════════════════════════════════════════════════
sec "9f. worker_args · numeric judges · over_iterations · wall_hours"

# ── worker_args: extra flags agg must hand the worker, in the right POSITION ──────────────
# `worker_args` lives under `defaults:` now (§4.1 — inheritable, the sandbox constraint). There is
# NO agg log line for it (worker.rs just appends them), so the only honest observation channel is
# the worker recording its own argv. Asserting on run.log would pass for the wrong reason.
WA="$(mkproj workerargs)"
cat > "$WA/bin/claude" <<'EOF'
#!/bin/sh
# the --version preflight must exit BEFORE we record, or it overwrites argv.txt with 1 token
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
: > argv.txt; for a in "$@"; do printf '%s\n' "$a" >> argv.txt; done
sh bin/rec RUN
: > did_work
git add did_work >/dev/null 2>&1
git commit -qm "worker: did_work" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$WA/bin/claude"
cat > "$WA/agg/agg.yaml" <<'EOF'
project: workerargs
defaults:
  model: fake
  worker_args: ["--allowedTools", "Edit,Bash", "--add-dir", "SENTINEL_SRC"]
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
hooks:
  on_session_start: ["sh bin/rec INJECT"]
  on_session_end: ["sh bin/rec GATE"]
EOF
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

# ── numeric judges: the scoreboard renders value/max, and a `.value` accessor gates on it ─────
# Goal TYPES are gone (§7.1): a judge emitting a `value` renders `value/max`; the number is read in
# a condition via the dotted `.value` accessor (§5.3 extension 1). `coverage.value >= 100 AND
# solved.value >= 28` is the DoD — a BARE `coverage >= 100` would be a hard error (a bool has no
# ordering against a number), which is exactly what the accessor fixes.
GT="$(mkproj numeric)"
cat > "$GT/agg/judges/coverage.sh" <<'EOF'
#!/bin/sh
sh bin/rec VERIFY
n=$(cat .n 2>/dev/null || echo 0)
if [ "$n" -ge 1 ]; then echo '{"met":true,"value":100,"max":100,"target":100,"rationale":"done"}'
else echo '{"met":false,"value":50,"max":100,"target":100,"rationale":"halfway"}'; fi
EOF
cat > "$GT/agg/judges/solved.sh" <<'EOF'
#!/bin/sh
n=$(cat .n 2>/dev/null || echo 0)
if [ "$n" -ge 1 ]; then echo '{"met":true,"value":28,"max":28,"target":28,"rationale":"all 28"}'
else echo '{"met":false,"value":18,"max":28,"target":28,"rationale":"18 of 28"}'; fi
EOF
chmod +x "$GT/agg/judges/coverage.sh" "$GT/agg/judges/solved.sh"
cat > "$GT/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
echo 1 > .n
git add .n >/dev/null 2>&1
git commit -qm "worker: .n" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$GT/bin/claude"
cat > "$GT/agg/agg.yaml" <<'EOF'
project: numeric
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "coverage.value >= 100 AND solved.value >= 28"
summary: { enabled: false }
hooks:
  on_session_start: ["sh bin/rec INJECT"]
  on_session_end: ["sh bin/rec GATE"]
EOF
agg_do "$GT" run --max-sessions 3 > "$GT/run.log" 2>&1
is  "numeric judges + a .value DoD drive the loop to stop (exit 0)" "$?" "0"
has "…baseline renders the coverage measure as value/max" "$GT/run.log" "50/100"
has "…and the solved measure"                             "$GT/run.log" "18/28"
has "…coverage reaches its full value"                    "$GT/run.log" "100/100"
has "…and solved reaches its full value"                  "$GT/run.log" "28/28"
has "…and the compound .value DoD (AND) fires"            "$GT/run.log" "2/2 goals met"

# ── over_iterations: a GUARD (exit 3), distinct from the max-sessions cap (exit 4) ───────
# stop.rs — sessions_done >= max_sessions. It is evaluated in GATE, so it halts BEFORE the loop's
# own top-of-cycle max-sessions pre-check ever fires. (over_iterations reads the SAME ceiling the
# --max-sessions flag sets, §4.1.)
OI="$(mkproj overiter)"; : > "$OI/NO_WORK"
cat > "$OI/agg/agg.yaml" <<'EOF'
project: overiter
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
  abort_if: "over_iterations"
summary: { enabled: false }
EOF
agg_do "$OI" run --max-sessions 2 > "$OI/run.log" 2>&1
is    "over_iterations HALTS the loop (exit 3, a guard — not the exit-4 cap)" "$?" "3"
has   "…and names the guard"                 "$OI/run.log" "over_iterations"
hasnt "…the max-sessions cap never fired"    "$OI/run.log" "reached max_sessions"

# ── wall_hours: a raw counter usable in any abort expression (stop.rs) ────────────────────
WH="$(mkproj wallhours)"; : > "$WH/NO_WORK"; echo 4 > "$WH/WORKER_SLEEP"
# baseline sits at ~0.00001h; one 4s session puts wall_hours at ~0.0012h.
cat > "$WH/agg/agg.yaml" <<'EOF'
project: wallhours
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
  abort_if: "wall_hours >= 0.0005"
summary: { enabled: false }
EOF
agg_do "$WH" run --max-sessions 5 > "$WH/run.log" 2>&1
is    "a wall_hours ceiling HALTS the loop (exit 3)" "$?" "3"
has   "…and names the expression"                    "$WH/run.log" "wall_hours"
hasnt "…it did not simply run out of sessions"       "$WH/run.log" "reached max_sessions"

# ═══════════════════════════════════════════════════════════════════════════
sec "9g. the git paths the rollback gate does NOT take (auto-accept · conflict · recovery)"

# ── gate_regressions:false → auto-accept: a regressing session is KEPT, never rolled back ───────
# The loop ALWAYS stages+gates now (the old eager `resolve_session` is unreachable); `gate_regressions:
# false` just makes the gate keep every session. A regressing session therefore LANDS on base — the
# opposite of the default gate — which is the whole point of the knob.
EM="$(mkproj eager)"
cat > "$EM/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
n=$(cat .sess 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > .sess
echo "sess-$n" > tracked.txt
git add tracked.txt && git commit -qm "worker: session $n"
: > did_work
[ "$n" -ge 2 ] && : > JUDGE_FAIL     # session 2 regresses — but gate_regressions:false keeps it
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$EM/bin/claude"
cat > "$EM/agg/judges/endless.sh" <<'EOF'
#!/bin/sh
echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"keeps the loop alive"}'
EOF
chmod +x "$EM/agg/judges/endless.sh"
cat > "$EM/agg/agg.yaml" <<'EOF'
project: eager
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "endless"
  invariants: [worked]
  gate_regressions: false
summary: { enabled: false }
EOF
echo base > "$EM/tracked.txt"
gitbase "$EM"
agg_do "$EM" run --max-sessions 2 > "$EM/run.log" 2>&1
has  "gate_regressions:false keeps every session"       "$EM/run.log" "session #1 merged → kept"
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
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$MC/bin/claude"
cat > "$MC/agg/agg.yaml" <<'EOF'
project: conflict
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
EOF
echo base > "$MC/tracked.txt"
gitbase "$MC"
agg_do "$MC" run --max-sessions 1 > "$MC/run.log" 2>&1
has "a conflicting merge FAILS loudly"              "$MC/run.log" "FAILED (conflict)"
has "…and the branch is kept for inspection"        "$MC/run.log" "kept for inspection"
is  "…base is left exactly as it was"               "$( cd "$MC" && git show main:tracked.txt 2>/dev/null )" "base-side"
( cd "$MC" && git rev-parse -q --verify MERGE_HEAD >/dev/null ) \
  && bad "the failed merge left MERGE_HEAD behind" \
  || ok "…and no MERGE_HEAD is stranded (the merge was aborted)"

# ── startup recovery of a merge stranded by an interrupted run ───────────────────────────
# The discriminator is .git/MERGE_MSG (git.rs): agg's own merge names the branch_prefix.
# GOTCHA: recovery runs BEFORE the baseline pass; the goal need not be met, the strand just has
# to exist. The default worker (creates + commits did_work) then drives the run to done.
strand() { # strand <dir> <branch-name>  → leaves a conflicted, uncommitted merge
  # both sides must differ from the committed "base", or `git commit` finds nothing to do
  # and the `&&` chain dies before the merge ever runs.
  ( cd "$1" && git checkout -q -b "$2" && echo branch-side > tracked.txt && git commit -qam b \
     && git checkout -q main && echo main-side > tracked.txt && git commit -qam m \
     && git merge --no-commit "$2" >/dev/null 2>&1; true )
}
SR="$(mkproj recover)"
cat > "$SR/agg/agg.yaml" <<'EOF'
project: recover
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
EOF
echo base > "$SR/tracked.txt"; gitbase "$SR"
strand "$SR" "agg/recover/session-1"      # name contains the `agg` branch_prefix
( cd "$SR" && git rev-parse -q --verify MERGE_HEAD >/dev/null ) && ok "fixture: a merge is genuinely stranded" || bad "fixture failed to strand a merge"
agg_do "$SR" run --max-sessions 1 > "$SR/run.log" 2>&1
has "agg recovers its OWN stranded merge at startup" "$SR/run.log" "found a leftover staged merge from an interrupted session"
has "…so isolation still turns ON"                   "$SR/run.log" "per-session branch isolation ON"
( cd "$SR" && git rev-parse -q --verify MERGE_HEAD >/dev/null ) \
  && bad "MERGE_HEAD survived recovery" || ok "…and MERGE_HEAD is cleared"

# a merge agg did NOT start: it is left alone, and because isolation is MANDATORY and needs a clean
# tree, agg REFUSES to start (rather than the old "disable isolation, run on current branch" — that
# escape hatch is gone). The user's merge survives untouched.
SU="$(mkproj unrelated)"
cat > "$SU/agg/agg.yaml" <<'EOF'
project: unrelated
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
EOF
echo base > "$SU/tracked.txt"; gitbase "$SU"
strand "$SU" "hotfix/urgent"              # no `agg` anywhere in the merge message
agg_do "$SU" run --max-sessions 1 > "$SU/run.log" 2>&1
is  "…agg refuses to start (a foreign merge is a dirty tree)" "$?" "1"
has "a merge agg did NOT start is left alone, with a warning" "$SU/run.log" "WARNING a merge is in progress that agg did not start"
has "…and isolation refuses rather than trample it"          "$SU/run.log" "uncommitted tracked changes"
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
mkdir -p agg/state/memory
i=0; while [ $i -lt 40 ]; do printf 'padding line %s for session %s\n' "$i" "$n" >> "agg/state/memory/session-$n.md"; i=$((i+1)); done
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$MK/bin/claude"
cat > "$MK/agg/agg.yaml" <<'EOF'
project: memcap
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
memory: { enabled: true, max_kb: 1, inject_kb: 1 }
hooks:
  on_session_start: ["sh bin/rec INJECT"]
  on_session_end: ["sh bin/rec GATE"]
EOF
agg_do "$MK" run --max-sessions 4 > "$MK/run.log" 2>&1
exists "AGG_MEMORY.md exists after 4 sessions" "$MK/agg/state/AGG_MEMORY.md"
SZ=$(wc -c < "$MK/agg/state/AGG_MEMORY.md" | tr -d ' ')
printf '  AGG_MEMORY.md = %s bytes (cap = 1 KB)\n' "$SZ"
[ "$SZ" -le 1100 ] && ok "…and max_kb=1 caps the durable file (${SZ}B)" \
                   || bad "max_kb not enforced" "${SZ}B > 1 KB"
has "…dropping the OLDEST entries, and saying so" "$MK/agg/state/AGG_MEMORY.md" "older entries dropped"
has "…the newest session survives the rotation"   "$MK/agg/state/AGG_MEMORY.md" "session 4"
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
sec "9i. the rest of the surface (effort · base_branch · invariants · judge.timeout · env · flags)"

# ── effort: passed through to the worker as `--effort <value>` (worker.rs) ────────────────
# `effort` lives under `defaults:` now (§4.1).
EF="$(mkproj effort)"
cat > "$EF/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
: > argv.txt; for a in "$@"; do printf '%s\n' "$a" >> argv.txt; done
sh bin/rec RUN
: > did_work
git add did_work >/dev/null 2>&1
git commit -qm "worker: did_work" >/dev/null 2>&1
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$EF/bin/claude"
cat > "$EF/agg/agg.yaml" <<'EOF'
project: effort
defaults: { model: fake, effort: low }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
hooks:
  on_session_start: ["sh bin/rec INJECT"]
  on_session_end: ["sh bin/rec GATE"]
EOF
agg_do "$EF" run --max-sessions 2 > "$EF/run.log" 2>&1
has "effort is handed to the worker as --effort" "$EF/argv.txt" "--effort"
has "…with the configured value"                 "$EF/argv.txt" "low"

# ── session_isolation.base_branch: cut sessions from a branch that is NOT the current one ─
BB="$(mkproj basebranch)"
cat > "$BB/agg/agg.yaml" <<'EOF'
project: basebranch
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
session_isolation: { base_branch: "trunk" }
EOF
echo base > "$BB/tracked.txt"; gitbase "$BB"
( cd "$BB" && git branch trunk )
agg_do "$BB" run --max-sessions 1 > "$BB/run.log" 2>&1
has "base_branch overrides the launch branch" "$BB/run.log" "base branch 'trunk'"
has "…and sessions are cut off it"            "$BB/run.log" "(off trunk)"

# ── invariants + any_regressed(invariants) as an ABORT ───────────────────────────────────
# With the default gate a regression is rolled back (any_regressed then reads the RESTORED state and
# never fires). To exercise any_regressed(invariants) as an abort we turn the gate OFF, so the
# regression LANDS and the term can see it. The worker commits its break so it stages (an
# uncommitted break is NoChanges and would be undone).
IV="$(mkproj invariant)"
cat > "$IV/agg/judges/safe.sh" <<'EOF'
#!/bin/sh
if [ -f BREAK ]; then echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"safety broke"}'
else echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"safe"}'; fi
EOF
cat > "$IV/agg/judges/endless.sh" <<'EOF'
#!/bin/sh
echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"keeps the loop alive"}'
EOF
chmod +x "$IV/agg/judges/safe.sh" "$IV/agg/judges/endless.sh"
cat > "$IV/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
echo broke > BREAK           # the worker breaks the invariant it was told to preserve
git add BREAK && git commit -qm "worker: break"
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$IV/bin/claude"
cat > "$IV/agg/agg.yaml" <<'EOF'
project: invariant
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "endless"
  invariants: [safe]
  abort_if: "any_regressed(invariants)"
  gate_regressions: false
summary: { enabled: false }
EOF
echo base > "$IV/tracked.txt"; gitbase "$IV"
agg_do "$IV" run --max-sessions 3 > "$IV/run.log" 2>&1
is    "a regressed INVARIANT halts the loop (exit 3)"  "$?" "3"
has   "…naming the guard"                              "$IV/run.log" "any_regressed"
hasnt "…and it is not the session cap"                 "$IV/run.log" "reached max_sessions"

# ── judge.timeout: a hanging judge is killed by the run-level timeout; the loop survives ──
# The per-judge timeout moved to the run-level `judge:` block (§5.2). A judge that hangs past it is
# killed → Verdict::failed (an error, NOT a clean not-met), so it never meets and never regresses;
# the loop simply reaches the cap.
JT="$(mkproj judgetimeout)"; : > "$JT/NO_WORK"
cat > "$JT/agg/judges/worked.sh" <<'EOF'
#!/bin/sh
sleep 30
echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"never gets here"}'
EOF
chmod +x "$JT/agg/judges/worked.sh"
cat > "$JT/agg/agg.yaml" <<'EOF'
project: judgetimeout
defaults: { model: fake }
judge: { timeout: 1 }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
EOF
JTS=$(date +%s)
agg_do "$JT" run --max-sessions 1 > "$JT/run.log" 2>&1
is "a judge that hangs does not hang the loop (exit 4, the cap)" "$?" "4"
[ $(( $(date +%s) - JTS )) -lt 25 ] && ok "…the judge timeout fired instead of waiting it out" \
                                    || bad "the judge ran to completion; timeout ignored"
hasnt "…and the hung judge never reports met" "$JT/run.log" "never gets here"

# ── AGG_MEMORY_MAX_KB env override (config.rs apply_env_overrides) ────────────────────────
EV="$(mkproj memenv)"; : > "$EV/NO_WORK"
cat > "$EV/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
sh bin/rec RUN
n=$(cat .sess 2>/dev/null || echo 0); n=$((n+1)); echo "$n" > .sess
mkdir -p agg/state/memory
i=0; while [ $i -lt 200 ]; do printf 'padding line %s of session %s\n' "$i" "$n" >> "agg/state/memory/session-$n.md"; i=$((i+1)); done
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$EV/bin/claude"
# CONTROL first: without the override the file must exceed 1 KB, or the assertion below is
# vacuous (it would "pass" simply because nothing was ever big enough to cap).
( cd "$EV" && PATH="$EV/bin:$PATH" "$AGG" run --max-sessions 4 > uncapped.log 2>&1 )
RAW=$(wc -c < "$EV/agg/state/AGG_MEMORY.md" 2>/dev/null | tr -d ' ')
[ "${RAW:-0}" -gt 1100 ] && ok "control: uncapped memory really does exceed 1 KB (${RAW}B)" \
                         || bad "control failed — the memcap assertion would be vacuous" "${RAW}B"
rm -f "$EV/.sess"; rm -rf "$EV/agg/state"
( cd "$EV" && PATH="$EV/bin:$PATH" AGG_MEMORY_MAX_KB=1 "$AGG" run --max-sessions 4 > run.log 2>&1 )
SZ=$(wc -c < "$EV/agg/state/AGG_MEMORY.md" 2>/dev/null | tr -d ' ')
[ "${SZ:-99999}" -le 1100 ] && ok "AGG_MEMORY_MAX_KB=1 overrides the config default (${RAW}B → ${SZ}B)" \
                            || bad "the env override was ignored" "${SZ}B"
has "…and the rotation notice proves the cap actually fired" "$EV/agg/state/AGG_MEMORY.md" "older entries dropped"

# ── global flags: --dir, --config (--goals is GONE, §7.1) ─────────────────────────────────
GF="$(mkproj globalflags)"
( cd "$WS" && PATH="$GF/bin:$PATH" "$AGG" --dir "$GF" run --max-sessions 2 > "$GF/dirrun.log" 2>&1 )
is  "--dir runs the loop in another directory (exit 0)" "$?" "0"
exists "…and the worker really worked there"           "$GF/did_work"

GC="$(mkproj cfgflags)"
mv "$GC/agg/agg.yaml" "$GC/custom.yaml"        # the config file at a NON-default path; judges still resolve from agg/judges/
agg_do "$GC" --config "$GC/custom.yaml" run --max-sessions 2 > "$GC/run.log" 2>&1
is  "--config accepts a non-default filename (exit 0)" "$?" "0"
has "…and the run really reached its Definition of Done" "$GC/run.log" "done_if satisfied"

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
sec "9k. the sequence/step model, observed end-to-end (§5.4 · §5.7 · §4.1)"

# ── `worker x4` — a repeat runs the step four times before the sequence wraps ────────────
RX="$(mkproj repeatx)"; : > "$RX/NO_WORK"
cat > "$RX/agg/agg.yaml" <<'EOF'
project: repeatx
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps:
    - worker x4
  done_if: "worked"
summary: { enabled: false }
hooks:
  on_session_start: ["sh bin/rec INJECT"]
EOF
agg_do "$RX" run --max-sessions 4 > "$RX/run.log" 2>&1
# NB: the backticks below MUST be escaped. Unescaped, bash runs `worker x4` as a command
# substitution while expanding this line's args — that clobbers $? (to 127, "command not found")
# BEFORE the "$?" arg is read, so the assertion would test the wrong exit code entirely.
is  "a \`worker x4\` repeat runs to the session cap (exit 4, DoD never met)" "$?" "4"
RUNS=$(grep -c 'RUN=run' "$RX/trace.txt" 2>/dev/null || echo 0)
[ "$RUNS" -eq 4 ] && ok "…dispatching the worker step exactly 4 times" \
                  || bad "\`worker x4\` ran the step $RUNS times, expected 4"

# ── `if <cond> then <step>` — the branch fires its step only when the condition is met ────
# `flip` is a run-set-only control judge (named ONLY in the if-condition): met once .flip is
# COMMITTED, which the FIRST worker session does (uncommitted work is NoChanges and the gate would
# restore flip to not-met, so the branch would never fire). So session 1 = worker; session 2 = the
# branch target `reconsider` (skip_judges), which STAGES. A per-step override proves reconsider ran:
# it names a distinct `prompt:` that lands in that worker's prompt additively (§5.6). We assert
# against prompts.txt (which accumulates every prompt) so the check is independent of session order.
BR="$(mkproj branch)"
cat > "$BR/agg/judges/flip.sh" <<'EOF'
#!/bin/sh
[ -f .flip ] && echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"flipped"}' \
             || echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"not yet"}'
EOF
chmod +x "$BR/agg/judges/flip.sh"
cat > "$BR/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
prev=""; for a in "$@"; do [ "$prev" = "-p" ] && printf '%s\n===8<===\n' "$a" >> prompts.txt; prev="$a"; done
sh bin/rec RUN
if [ ! -f .flip ]; then : > .flip; git add .flip >/dev/null 2>&1; git commit -qm flip >/dev/null 2>&1; fi
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$BR/bin/claude"
cat > "$BR/agg/agg.yaml" <<'EOF'
project: branch
defaults: { model: fake }
steps:
  worker: {}
  reconsider: { skip_judges: true, prompt: "RECONSIDER_MARKER_QRS" }
sequence:
  steps:
    - worker
    - if flip then reconsider
  done_if: "worked"
summary: { enabled: false }
EOF
echo base > "$BR/tracked.txt"; gitbase "$BR"
agg_do "$BR" run --max-sessions 3 > "$BR/run.log" 2>&1
has "an `if <cond> then <step>` fires its target once the condition holds" "$BR/run.log" "(skip_judges) — nothing merged yet"
has "…and the branch target's per-step prompt reaches that worker"         "$BR/prompts.txt" "RECONSIDER_MARKER_QRS"

# ── a per-step AGENT override is honoured (the whole point: perspective diversity, §3) ────
# `build` overrides the worker agent to `codex`; the loop must launch THAT agent for that step. We
# shim BOTH `claude` and `codex` on PATH so the override is observable without a real second vendor.
PA="$(mkproj peragent)"; : > "$PA/NO_WORK"
cp "$PA/bin/claude" "$PA/bin/codex"
# make each shim announce which binary ran, so we can see the override took effect.
cat > "$PA/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
echo "AGENT=claude" >> whoran.txt
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
cat > "$PA/bin/codex" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
echo "AGENT=codex" >> whoran.txt
printf '{"type":"result","subtype":"success","is_error":false,"result":"d","usage":{"output_tokens":1},"total_cost_usd":0}\n'
EOF
chmod +x "$PA/bin/claude" "$PA/bin/codex"
cat > "$PA/agg/agg.yaml" <<'EOF'
project: peragent
defaults: { agent: claude, model: fake }
judge: { agent: claude, model: fake }
steps:
  plan: {}
  build: { agent: codex }
sequence:
  steps:
    - plan
    - build
  done_if: "worked"
summary: { enabled: false }
EOF
agg_do "$PA" run --max-sessions 2 > "$PA/run.log" 2>&1
has "the default step runs the default agent (claude)"     "$PA/whoran.txt" "AGENT=claude"
has "…and a per-step `agent:` override runs the other one (codex)" "$PA/whoran.txt" "AGENT=codex"

# ── deny_unknown_fields: a stray/mistyped config key is a HARD ERROR at startup (§4.1) ────
# The guard the config move depends on — a stale top-level `budget:` (its new home is under
# `sequence:`) must be REFUSED, not silently ignored (a decorative spend ceiling = an unbounded loop).
DK="$(mkproj denyunknown)"
cat > "$DK/agg/agg.yaml" <<'EOF'
project: denyunknown
defaults: { model: fake }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: "worked"
summary: { enabled: false }
budget: { total: 5 }
EOF
agg_do "$DK" run --max-sessions 1 > "$DK/run.log" 2>&1
# escaped backticks (see the `worker x4` note above): an unescaped `budget:` would run as a command
# substitution and clobber $? to 127 before the exit code is asserted.
is  "a stray top-level \`budget:\` (its home is under sequence:) is refused, not ignored" "$?" "1"
has "…naming the unknown field"                                                          "$DK/run.log" "unknown field \`budget\`"

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
  && ok "GET /api/state → live snapshot with a four-stage phase + the judge scoreboard" || bad "/api/state malformed"

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
UNKNOWN=$(grep -vE '^(inject|run|verify|gate|backoff|staging|starting|done)$' "$SV/phases.uniq" | tr '\n' ' ')
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
  # emit ~60 `assistant` text events so max_scroll > 0. It commits did_work so the run reaches done.
  cat > "$T/bin/claude" <<'EOF'
#!/bin/sh
for a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done
i=1; while [ $i -le 60 ]; do
  printf '{"type":"assistant","message":{"content":[{"type":"text","text":"thinking step %s"}]}}\n' "$i"
  i=$((i+1))
done
: > did_work
git add did_work >/dev/null 2>&1
git commit -qm "worker: did_work" >/dev/null 2>&1
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
  has "…paints the judge"           "$T/base.txt" "worked"
  has "…paints the phase field"     "$T/base.txt" "phase"
  has "…paints the worker's activity stream" "$T/base.txt" "thinking step"
  has "…paints the finished banner" "$T/base.txt" "FINISHED"
  has "…paints the keybinding help" "$T/base.txt" "q=quit"
  # focus starts on Activity, follow-mode starts on
  has "…Activity starts focused (▸)"        "$T/base.txt" "▸ Activity"
  has "…and auto-follow starts on (⏵live)"  "$T/base.txt" "⏵live"
  hasnt "…Judges is not focused to begin with" "$T/base.txt" "▸ Judges"
  hasnt "…and follow is not paused"           "$T/base.txt" "paused"

  # a user presses Tab to move focus, f to toggle follow, arrows to scroll
  drive tab "1.5:Tab,0.6:RESIZE,0.8:q" 25
  is  "Tab is accepted and the TUI still quits" "$?" "0"
  has "…Tab moves focus to Judges (▸ Judges)"   "$T/tab.txt" "▸ Judges"

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
sec "12. the web interface (SvelteKit BFF → agg serve → agg/state/)"
if [ "$WEB" = "0" ]; then
  skip "web interface" "--no-web"
elif ! command -v node >/dev/null 2>&1; then
  skip "web interface" "node not installed"
elif [ ! -d "$ROOT/src/web/node_modules" ]; then
  skip "web interface" "run 'npm install' in web/ first"
else
  WPORT=$(free_port); APORT=$(free_port)
  W="$(mkproj web)"; : > "$W/NO_WORK"; echo 3 > "$W/WORKER_SLEEP"

  ( cd "$ROOT/src/web" && npm run build > "$W/build.log" 2>&1 )
  is "web app builds (npm run build)" "$?" "0"

  agg_bg WSRV "$W" serve.log serve --port "$APORT" --cors-origin "http://localhost:$WPORT"
  ( cd "$ROOT/src/web" && exec env AGG_API="http://127.0.0.1:$APORT" PORT="$WPORT" node build/index.js > "$W/web.log" 2>&1 ) & WAPP=$!
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
  ( cd "$ROOT/src/web" && exec env AGG_API="http://127.0.0.1:1" PORT="$WPORT" node build/index.js > "$W/web2.log" 2>&1 ) & WAPP2=$!
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
sec "13. agg skills install — the /agg:* skills reach all three agents"
# The skills used to be a Claude-only plugin. Two things have to hold now, and they are
# different claims:
#   (a) the files land where each agent ACTUALLY looks   — asserted hermetically, below
#   (b) the agent then DISCOVERS them                    — asserted against the real CLIs, free
# and the one that actually matters:
#   (c) the agg.yaml recipes /agg:new is told to emit are configs that agg will START.
# (c) is the whole point of the phase: a setup skill that writes a config `capability::check`
# refuses is worse than no setup skill at all.

SK="$WS/skills"; mkdir -p "$SK/agg"
printf 'project: p\ndefaults: { agent: codex }\nsteps: { worker: {} }\nsequence: { steps: [worker] }\n' > "$SK/agg/agg.yaml"

# --- (a) the right directory per agent. Claude and the other two do NOT share one. ----------
"$AGG" --dir "$SK" skills install --agent claude  > "$SK/i-claude.log"  2>&1
"$AGG" --dir "$SK" skills install --agent codex   > "$SK/i-codex.log"   2>&1
"$AGG" --dir "$SK" skills install --agent copilot > "$SK/i-copilot.log" 2>&1
for s in new status supervise; do
  exists "claude finds agg-$s in .claude/skills/"  "$SK/.claude/skills/agg-$s/SKILL.md"
  exists "codex+copilot find agg-$s in .agents/skills/" "$SK/.agents/skills/agg-$s/SKILL.md"
done
# codex and copilot share `.agents/` — that is the entire reason two dirs cover three agents.
has "copilot installs to the SAME neutral dir as codex" "$SK/i-copilot.log" ".agents/skills"
# Codex/Copilot name a skill after its DIRECTORY (no `name:` key), so the dir carries the
# namespace. `new/` instead of `agg-new/` would surface as a skill called "new".
absent "the un-namespaced dir name is never used" "$SK/.agents/skills/new"
# the description is what Codex/Copilot ROUTE on — an empty copy would list a skill saying nothing
has "the installed skill keeps its frontmatter"  "$SK/.agents/skills/agg-new/SKILL.md" "description:"
has "…and the capability-aware agg.yaml template" "$SK/.agents/skills/agg-new/SKILL.md" "agent: <claude|codex|copilot>"

# --- the ergonomics: no flags needed, and a bad agent writes nothing ------------------------
D2="$WS/skills-default"; mkdir -p "$D2/agg"
printf 'project: p\ndefaults: { agent: copilot }\nsteps: { worker: {} }\nsequence: { steps: [worker] }\n' > "$D2/agg/agg.yaml"
"$AGG" --dir "$D2" skills install > "$D2/i.log" 2>&1
has "agg skills install defaults the agent to agg.yaml's 'agent:' key" "$D2/i.log" "for \`copilot\`"
exists "…and installs it where copilot looks" "$D2/.agents/skills/agg-new/SKILL.md"

D3="$WS/skills-bad"; mkdir -p "$D3"
"$AGG" --dir "$D3" skills install --agent gemini > "$D3/i.log" 2>&1
is  "an unknown agent exits non-zero" "$?" "1"
has "…naming the agents that DO exist" "$D3/i.log" "known agents: claude, codex, copilot"
absent "…and writing nothing at all"   "$D3/.agents"

# --user resolves against $HOME, not the project (HOME overridden so we never touch the real one)
HM="$WS/fakehome"; mkdir -p "$HM"
( cd "$SK" && HOME="$HM" "$AGG" skills install --agent codex --user > "$SK/i-user.log" 2>&1 )
exists "--user installs under \$HOME/.agents/skills/" "$HM/.agents/skills/agg-new/SKILL.md"
( cd "$SK" && HOME="$HM" "$AGG" skills install --agent claude --user > /dev/null 2>&1 )
exists "--user --agent claude installs under \$HOME/.claude/skills/" "$HM/.claude/skills/agg-new/SKILL.md"

# doctor reports the install (as a fact — never a failure; the skills are optional)
( cd "$SK" && "$AGG" doctor > "$SK/doc.log" 2>&1 )
has "agg doctor reports the skills are installed for the active agent" "$SK/doc.log" \
    "the /agg:* skills are installed for \`codex\`"
DN="$WS/skills-none"; mkdir -p "$DN/agg"
printf 'project: p\ndefaults: { agent: codex }\nsteps: { worker: {} }\nsequence: { steps: [worker] }\n' > "$DN/agg/agg.yaml"
( cd "$DN" && "$AGG" doctor > "$DN/doc.log" 2>&1 )
has "…and says so, non-fatally, when they are not" "$DN/doc.log" "are not installed for \`codex\`"

# --- (c) THE ONE THAT MATTERS: /agg:new's recipes must produce a config that STARTS ----------
# Each block below is exactly what the skill's Step-0 rules tell it to emit. If `agg doctor`
# refuses one of these, the skill is generating a config that `agg run` will not start — which
# is the precise failure this whole phase exists to prevent. (budget/cost live under `sequence:`
# now; only claude REPORTS `cost:` in dollars — so a cost guard is REFUSED on codex, but merely
# INERT-but-loud on copilot, which self-caps via `--max-ai-credits`. §4.1/§7.3.)
capcheck() { # capcheck <desc> <expect: ok|refused|inert> <agg.yaml body>
  local desc=$1 expect=$2 body=$3
  local d="$WS/cap-$(echo "$desc" | tr -cd '[:alnum:]' | cut -c1-16)"
  mkdir -p "$d/agg/judges"; printf '%s' "$body" > "$d/agg/agg.yaml"
  printf '#!/bin/sh\necho "{\\"met\\":true}"\n' > "$d/agg/judges/g.sh"; chmod +x "$d/agg/judges/g.sh"
  printf 'do work\n' > "$d/agg/AGG_STATE.md"
  ( cd "$d" && "$AGG" doctor > "$d/doc.log" 2>&1 )
  if grep -q "cannot do" "$d/doc.log"; then
    [ "$expect" = "refused" ] && ok "$desc" || bad "$desc" "agg REFUSED a config the skill tells /agg:new to write"
  elif [ "$expect" = "inert" ]; then
    # §7.3: the guard is accepted (the agent self-caps) but its inertness MUST be surfaced loudly —
    # an accepted-and-silent cost guard would be the exact quiet no-op this whole check forbids.
    grep -q "INERT" "$d/doc.log" && ok "$desc" \
      || bad "$desc" "agg accepted the inert cost guard but never WARNED it will not fire — a silent no-op"
  else
    [ "$expect" = "ok" ] && ok "$desc" || bad "$desc" "agg ACCEPTED a config the skill forbids — the guard is dead"
  fi
}
# the recipes the skill prescribes → must all start
capcheck "the skill's claude recipe starts"  ok \
  'project: p
defaults: { agent: claude, model: "claude-opus-4-8[1m]" }
judge: { agent: claude, model: haiku }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: g
  budget: { total: 100000 }
  cost: { total: 5.0 }
summary: { enabled: true }
'
capcheck "the skill's codex recipe starts (no model, no cost)" ok \
  'project: p
defaults: { agent: codex }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: g
  budget: { total: 100000 }
summary: { enabled: true }
'
capcheck "the skill's copilot recipe starts (model auto, no cost)" ok \
  'project: p
defaults: { agent: copilot, model: auto }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: g
  budget: { total: 100000 }
summary: { enabled: true }
'
# and the mistake the skill exists to prevent → a dollar guard on an agent that cannot report USD.
# §7.3 splits by whether the agent can still bound itself:
#   · codex has NO self-cap → the guard leaves the loop unbounded → REFUSED, loudly ("cannot do").
#   · copilot self-caps (--max-ai-credits) → the dollar guard is merely INERT → ACCEPTED, but the
#     inertness is surfaced LOUDLY (never a silent no-op). Both verified on the wire.
capcheck "a Claude-shaped cost guard on codex is REFUSED (no self-cap → unbounded)" refused \
  'project: p
defaults: { agent: codex }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: g
  cost: { total: 5.0 }
'
capcheck "a Claude-shaped cost guard on copilot is INERT-but-loud (it self-caps, §7.3)" inert \
  'project: p
defaults: { agent: copilot, model: auto }
steps: { worker: {} }
sequence:
  steps: [worker]
  done_if: g
  cost: { total: 5.0 }
'

# --- the OTHER silent-wrong-config trap: the skill must not GUESS the agent -----------------
# Copilot cannot introspect its own identity, and an early draft of the skill told it to "default
# to the agent you are running in". It wrote `agent: claude` + a claude-opus model. `agg doctor`
# PASSED (a Claude config is perfectly valid), and the loop would have silently driven the wrong
# agent. doctor cannot catch this — it has no idea which agent you MEANT. Only the detection
# snippet does, so assert the skill still carries it.
has "the skill tells the agent to DETECT itself, not guess" "$ROOT/plugin/skills/new/SKILL.md" 'COPILOT_CLI'
has "…via all three env markers"                            "$ROOT/plugin/skills/new/SKILL.md" 'CODEX_THREAD_ID'
has "…and forbids the silent claude fallback"               "$ROOT/plugin/skills/new/SKILL.md" 'There is no default'

# --- (b) the real agents actually DISCOVER what we installed --------------------------------
# Both probes below are FREE: `copilot skill list` is a local command and `codex debug
# prompt-input` renders the model-visible prompt without an API call. Copilot is on a limited
# free tier — do not "improve" these into `copilot -p` calls, which would spend its quota.
if command -v copilot >/dev/null 2>&1; then
  ( cd "$SK" && copilot skill list > "$SK/copilot-skills.txt" 2>&1 )
  for s in new status supervise; do
    has "copilot DISCOVERS agg-$s (real CLI, no quota)" "$SK/copilot-skills.txt" "agg-$s"
  done
else
  skip "copilot discovers the installed skills" "copilot not on PATH"
fi

if command -v codex >/dev/null 2>&1; then
  ( cd "$SK" && codex debug prompt-input > "$SK/codex-prompt.json" 2>/dev/null )
  for s in new status supervise; do
    has "codex DISCOVERS agg-$s (real CLI, no API call)" "$SK/codex-prompt.json" "agg-$s"
  done
  # …and it must be reading OUR project dir, not some stale personal install
  has "…from the project's .agents/skills/" "$SK/codex-prompt.json" ".agents/skills/agg-new/SKILL.md"
else
  skip "codex discovers the installed skills" "codex not on PATH"
fi

# --- (d) the PLUGIN MARKETPLACE route -------------------------------------------------------
# All three agents have a marketplace, and all three consume THIS repo's existing
# .claude-plugin/marketplace.json — Codex and Copilot read Claude's manifest format verbatim. So
# there is one plugin, not three, and nothing here needs a second manifest to maintain.
#
# The naming trap this guards: Copilot's plugin loader ignores the plugin namespace, so without an
# explicit `name:` key it surfaces plugin/skills/new/ as a skill called plain `new`. Claude's
# namespace still wins (/agg:new is unchanged), so the key is safe there. Assert both.
for s in new status supervise; do
  has "plugin skill $s declares name: agg-$s (or copilot calls it \"$s\")" \
      "$ROOT/plugin/skills/$s/SKILL.md" "name: agg-$s"
done
has "the marketplace manifest still points at plugin/" "$ROOT/.claude-plugin/marketplace.json" '"source": "./plugin"'

# Both live checks below run against a THROWAWAY agent home, so the suite never reads or mutates
# the developer's real marketplace registrations. (An earlier version did, and failed the moment a
# marketplace named `agenticgogo` was already registered from a different source.) Codex honours
# CODEX_HOME; Copilot keys off HOME. Both operations are local — no network, no model, no quota.
# codex bails with "failed to load configuration" if CODEX_HOME does not already exist.
MKT="$WS/agenthome"; mkdir -p "$MKT/codex" "$MKT/copilot"

if command -v codex >/dev/null 2>&1; then
  CODEX_HOME="$MKT/codex" codex plugin marketplace add "$ROOT" > "$SK/cx-mkt.log" 2>&1
  has "codex ACCEPTS our .claude-plugin/marketplace.json verbatim" "$SK/cx-mkt.log" "Added marketplace"
  CODEX_HOME="$MKT/codex" codex plugin add agg@agenticgogo > "$SK/cx-add.log" 2>&1
  has "…and installs the agg plugin from it" "$SK/cx-add.log" "Added plugin \`agg\`"
else
  skip "codex plugin marketplace" "codex not on PATH"
fi

if command -v copilot >/dev/null 2>&1; then
  HOME="$MKT/copilot" copilot plugin marketplace add "$ROOT" > "$SK/cp-mkt.log" 2>&1
  has "copilot ACCEPTS the SAME manifest (one plugin, not three)" "$SK/cp-mkt.log" "added successfully"
  HOME="$MKT/copilot" copilot plugin install agg@agenticgogo > "$SK/cp-add.log" 2>&1
  # "Installed 3 skills" is the assertion that matters: all three, not just the one it found first.
  has "…and installs all 3 skills from it" "$SK/cp-add.log" "Installed 3 skills"
else
  skip "copilot plugin marketplace" "copilot not on PATH"
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
