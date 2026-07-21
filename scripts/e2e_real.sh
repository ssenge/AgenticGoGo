#!/usr/bin/env bash
# REAL end-to-end acceptance: `agg` driving the ACTUAL `claude` CLI against a real model.
#
# scripts/e2e.sh stubs the worker so it is fast, free and deterministic. This one does not.
# It spends real tokens, and it is the only test that exercises the worker integration for
# real: the live stream-json shapes, the real `total_cost_usd` and `usage.output_tokens`, the
# real activity events, and a real agent actually satisfying an external judge — with agg
# auto-committing its work so mandatory session isolation keeps it.
#
# The `claude` on PATH is a PASSTHROUGH WRAPPER, not a stub: it records argv, records the
# phase agg had published when the worker started, and then `exec`s the real binary. Nothing
# about the model's behaviour is faked.
#
#   ./scripts/e2e_real.sh                          # ~1 min
#   ./scripts/e2e_real.sh --model claude-sonnet-5
#   KEEP=1 ./scripts/e2e_real.sh                   # keep the workspace
#
# The `usage (API-eq)` figure it prints is `total_cost_usd` as the CLI reports it: the
# API-equivalent list price of the work. On a Max/Pro subscription you are NOT charged it —
# it is the same number `sequence.limits.cost` / `over_cost` gate on. See README "usage (API-eq)".
#
# Config is the CURRENT judge/step model: a judge IS a goal (agg/judges/<name>.{sh,md}), the DoD
# is `done_if`, ceilings live in `sequence.limits`, and continuity across sessions is carried by
# COMMITTED git state + LOG.md — there is no `--resume` (every worker session is fresh). The worker's
# whole `-p` is a tiny pointer at agg/state/INSTRUCTIONS.md (agg regenerates it each session); this
# test proves a REAL agent reads that brief AND the STATE.md it points at, then does the work.
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
snap() { python3 -c "import json;print(json.load(open('$1/agg/state/state.json'))['$2'])" 2>/dev/null; }

trap '[ -n "${KEEP:-}" ] || rm -rf "$WS"' EXIT
mkdir -p "$WS"

REAL_CLAUDE="$(command -v claude || true)"
[ -x "$REAL_CLAUDE" ] || { echo "claude not on PATH"; exit 1; }

printf '\033[1mAgenticGoGo — REAL-model e2e\033[0m\n'
printf 'model: %s   claude: %s\nworkspace: %s\n' "$MODEL" "$REAL_CLAUDE" "$WS"
printf '\033[33mthis spends real subscription usage (not dollars).\033[0m\n'
( cd "$ROOT" && cargo build --quiet ) || { bad "cargo build"; exit 1; }

# ── fixture: a passthrough-instrumented `claude` + a named external judge + a git repo ────────
# mkproj <name> <judge_name> <done_if_expr> <state/STATE.md seed> [<step prompt — the persistent ask>]
# Writes agg/agg.yaml (judge/step model), agg/state/STATE.md, agg/judges/<judge_name>.sh stub, and
# inits a git repo (session isolation is mandatory). The section fills in the judge body after.
#
# The 4th arg seeds STATE.md (worker-curated forward advice; the worker REWRITES it each session).
# A persistent MULTI-session ask must therefore go in the 5th arg → the step `prompt:` (inlined into
# every session's brief, immutable config the worker cannot overwrite). A one-shot ask can just live
# in the STATE seed (§3: STATE is pointed-at, so this also proves a real agent follows that pointer).
mkproj() {
  local d="$WS/$1"; mkdir -p "$d/bin" "$d/agg/judges" "$d/agg/state"
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
printf '%s=%s\n' "$1" "$(sed -n 's/.*"phase":"\([a-z]*\)".*/\1/p' agg/state/state.json)" >> trace.txt
EOF
  chmod +x "$d/bin/claude" "$d/rec"
  { printf 'project: %s\n' "$1"
    printf 'defaults: { agent: claude, model: %s, state: state/STATE.md }\n' "$MODEL"
    printf 'judge: { agent: claude, model: %s, timeout: 120 }\n' "$MODEL"
    if [ -n "${5:-}" ]; then
      # a persistent ask → the step `prompt:` (a YAML block scalar), inlined into every brief.
      printf 'steps:\n  worker:\n    prompt: |\n'
      printf '%s\n' "$5" | sed 's/^/      /'
    else
      printf 'steps: { worker: {} }\n'
    fi
    printf 'summary: { enabled: false }\n'
    printf 'hooks:\n  on_session_start: ["sh ./rec INJECT"]\n  on_session_end: ["sh ./rec GATE"]\n'
    printf 'sequence:\n'
    printf '  steps: [ worker ]\n'
    printf '  limits: { cost: 1.0 }\n'
    printf '  done_if: "%s"\n' "$3"
    printf '  abort_if: "over_cost OR over_iterations"\n'
  } > "$d/agg/agg.yaml"
  printf '%s' "$4" > "$d/agg/state/STATE.md"
  # GIT_REDESIGN: agg now `git add -A`s the worker's WORK, so anything untracked in the tree gets
  # swept onto the session branch. Gitignore the scaffolding + instrumentation + the `*.log` capture
  # files agg's own stdout is redirected into (out.log/status.log) — committing an actively-written
  # log breaks the next `checkout base`. Only the real agent's WORK (answer.txt, count.txt) stays
  # trackable → agg commits + merges it, exactly as in production.
  cat > "$d/.gitignore" <<'EOF'
agg/state/
bin/
rec
agg/agg.yaml
agg/judges/
claude_args.txt
trace.txt
*.log
EOF
  ( cd "$d" && git init -q -b main && git config user.email t@t && git config user.name t \
      && git add -A && git commit -q -m seed )
  echo "$d"
}
run_agg() { local d=$1; shift; ( cd "$d" && PATH="$d/bin:$PATH" "$AGG" "$@" ); }

# ═════════════════════════════════════════════════════════════════════════════════════════
sec "1. a real agent, driven by agg, satisfying a real external judge (agg auto-commits its work)"
A="$(mkproj oneshot answered 'answered' 'Create a file named `answer.txt` in the current directory whose entire contents are the number 42 followed by a newline. Nothing else. Then stop. Do NOT run git — agg version-controls and commits your work for you.
')"
cat > "$A/agg/judges/answered.sh" <<'EOF'
#!/bin/sh
sh ./rec VERIFY
if [ -f answer.txt ] && grep -qx '42' answer.txt; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"answer.txt contains 42"}'
else
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"answer.txt missing or wrong"}'
fi
EOF
chmod +x "$A/agg/judges/answered.sh"

T0=$(date +%s)
run_agg "$A" run --max-sessions 3 > "$A/out.log" 2>&1
RC=$?
E1=$(( $(date +%s) - T0 ))
printf '  (%ss)\n' "$E1"
is "the loop reaches its Definition of Done (exit 0)" "$RC" "0"
has "…and says so (done_if satisfied)"            "$A/out.log" "done_if satisfied"
exists "the REAL agent created the file"           "$A/answer.txt"
is "…with exactly the content the judge demands"   "$(cat "$A/answer.txt" 2>/dev/null)" "42"
has "…and the session merged (agg auto-committed the worker's edit → kept)" "$A/out.log" "merged → kept"

sec "2. the four outer-loop stages, observed on a real run"
TRACE=$(tr '\n' ' ' < "$A/trace.txt" 2>/dev/null)
printf '  trace: %s\n' "$TRACE"
# baseline judging fires VERIFY once before session 1, then INJECT→RUN→VERIFY→GATE per session.
has "a baseline VERIFY runs before the first session" "$A/trace.txt" "VERIFY="
has "…then INJECT (on_session_start hook)"            "$A/trace.txt" "INJECT="
has "…then RUN (the real worker launches)"            "$A/trace.txt" "RUN="
has "…then GATE (on_session_end hook)"                "$A/trace.txt" "GATE="
is "the run settles on phase=done" "$(snap "$A" phase)" "done"

sec "3. real worker accounting (what a stub can never prove)"
# `cost_spent` is the CLI's `total_cost_usd`: the API-EQUIVALENT list price of the work, which
# the CLI reports on a subscription too. It is NOT a charge — it is what `over_cost` gates on.
TOK=$(snap "$A" tokens_spent); COST=$(snap "$A" cost_spent)
printf '  output tokens=%s   usage (API-eq)=$%s\n' "$TOK" "$COST"
[ "${TOK:-0}" -gt 0 ] 2>/dev/null && ok "output tokens parsed from the real result event" \
  || bad "tokens_spent is 0 — real usage.output_tokens not read"
python3 -c "import sys;sys.exit(0 if float('${COST:-0}') > 0 else 1)" \
  && ok "usage (API-eq) parsed from the real total_cost_usd — what over_cost gates on" \
  || bad "cost_spent is 0 — real total_cost_usd not read"
has "…and the session-exit line reports both" "$A/out.log" "out-tok"

sec "4. real stream-json parsing"
python3 - "$A" <<'PY'
import json, sys
d = json.load(open(sys.argv[1] + "/agg/state/state.json"))
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
hasnt "…and NO --resume (every session is fresh context — the moat)" "$A/claude_args.txt" "--resume"

sec "6. durable side effects of a real run"
exists "institutional memory was written" "$A/agg/state/LOG.md"
has    "…recording the real session"      "$A/agg/state/LOG.md" "## session 1"
is "the ledger is finalized as goals-met" \
   "$(python3 -c "import json;print(json.load(open('$A/agg/state/project.json'))['runs'][-1]['end_reason'])" 2>/dev/null)" "goals-met"
[ ! -f "$A/agg/state/run.pid" ] && ok "run.pid cleared by the Drop guard" || bad "run.pid left behind"
run_agg "$A" status > "$A/status.log" 2>&1
has "agg status renders the finished real run" "$A/status.log" "done"
is "…the agg-committed file is on main (agg committed the worker's edit, isolation merged it)" \
   "$(cd "$A" && git show main:answer.txt 2>/dev/null | tr -d '[:space:]')" "42"

# ═════════════════════════════════════════════════════════════════════════════════════════
sec "7. TWO real sessions: fresh-context continuity via COMMITTED state + memory (no --resume)"
# a counter that needs exactly two increments — the loop MUST take two sessions, and session 2
# (a FRESH context) must pick up session 1's committed count.txt + the folded memory. This is the
# whole point of the rewrite: continuity carried by git + LOG.md, not a --resume handle.
B="$(mkproj resume counted 'counted' \
'First session — count.txt does not exist yet. Follow the step task, then rewrite this note with the new count.' \
'Read the file `count.txt` in the current directory (if it does not exist, treat its value as 0).
Increment that number by exactly ONE. Write the new number back to `count.txt` as the only
contents, followed by a newline. Increment exactly once, then stop. Do not skip ahead. Do NOT run
git — agg version-controls and commits your work for you.')"
cat > "$B/agg/judges/counted.sh" <<'EOF'
#!/bin/sh
sh ./rec VERIFY
n=$(cat count.txt 2>/dev/null | tr -d '[:space:]')
if [ "$n" = "2" ]; then
  echo '{"met":true,"value":2,"max":2,"target":2,"rationale":"count reached 2"}'
else
  printf '{"met":false,"value":0,"max":2,"target":2,"rationale":"count is %s"}\n' "${n:-0}"
fi
EOF
chmod +x "$B/agg/judges/counted.sh"

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
hasnt "…and did so with NO --resume — pure fresh context each session" "$B/claude_args.txt" "--resume"
COUNT_FOLDS=$(grep -c "^## session" "$B/agg/state/LOG.md" 2>/dev/null || echo 0)
[ "$COUNT_FOLDS" -ge 2 ] && ok "memory folded one entry per real session ($COUNT_FOLDS)" \
                         || bad "expected ≥2 memory entries, got $COUNT_FOLDS"
has "…and session #1's record was INJECTed into session 2's prompt" "$B/out.log" "[memory] session #1 folded"
is "…the committed count on main reached 2 (each session's work merged)" \
   "$(cd "$B" && git show main:count.txt 2>/dev/null | tr -d '[:space:]')" "2"

# ═════════════════════════════════════════════════════════════════════════════════════════
sec "8. a real worker builds an OKF LLM wiki — driven ONLY by agg's standing footer, not the task"
# The task NEVER mentions the wiki; the only nudge is agg's built-in INSTRUCTIONS footer. This proves
# the shipped guidance actually drives a real worker to produce a LINKED OKF knowledge base. Runs on a
# CAPABLE model (design OQ5: weak workers curate the wiki unreliably — override with WIKI_MODEL), on a
# 2-op task forcing ≥2 sessions so a multi-session PLAN page is natural. Adds ~2 real sessions of cost.
WIKI_MODEL="${WIKI_MODEL:-claude-sonnet-5}"
W="$(mkproj wiki wikidone 'wikidone' \
'First session — calc.py does not exist yet.' \
'Build calc.py with two functions, add(a, b) and subtract(a, b), each returning the result.
Implement EXACTLY ONE not-yet-present function this session — real code plus a one-line print
self-test at the bottom — run it. Only ONE function per session, then stop. Do NOT run git — agg
commits your work for you.')"
cat > "$W/agg/judges/wikidone.sh" <<'EOF'
#!/bin/sh
sh ./rec VERIFY
n=0; for f in add subtract; do grep -qE "def[[:space:]]+$f" calc.py 2>/dev/null && n=$((n+1)); done
[ "$n" -ge 2 ] && printf '{"met":true,"value":%s,"max":2,"target":2,"rationale":"%s/2 ops"}\n' "$n" "$n" \
             || printf '{"met":false,"value":%s,"max":2,"target":2,"rationale":"%s/2 ops"}\n' "$n" "$n"
EOF
chmod +x "$W/agg/judges/wikidone.sh"
# worker → a capable model (wiki curation needs it); judge stays on $MODEL. Give sonnet cost headroom.
sed -i.bak "s#model: $MODEL, state#model: $WIKI_MODEL, state#; s#cost: 1.0#cost: 5.0#" "$W/agg/agg.yaml"

T0=$(date +%s); run_agg "$W" run --max-sessions 4 > "$W/out.log" 2>&1; E3=$(( $(date +%s) - T0 ))
WK="$W/agg/state/wiki"
printf '  worker model: %s   wiki pages: %s\n' "$WIKI_MODEL" "$(ls "$WK"/*.md 2>/dev/null | wc -l | tr -d ' ')"
[ -n "$(ls "$WK"/*.md 2>/dev/null)" ] && ok "the worker created pages under agg/state/wiki/ (from the footer alone)" \
                                      || bad "no wiki pages — the standing footer did not drive wiki creation"
grep -rlE '^type:' "$WK" >/dev/null 2>&1 && ok "…in OKF form (a \`type:\` frontmatter)" \
                                         || bad "wiki pages lack the OKF \`type:\` frontmatter"
grep -rhoE '\]\([a-z0-9._/-]+\.md\)' "$WK" 2>/dev/null | grep -q . \
  && ok "…and CROSS-LINKED with standard markdown links (a real graph, not isolated notes)" \
  || bad "wiki pages are not cross-linked — no standard [..](*.md) links"

TOTAL=$(python3 -c "print(round(float('$(snap "$A" cost_spent)') + float('$(snap "$B" cost_spent)') + float('$(snap "$W" cost_spent)'), 4))" 2>/dev/null)
printf '\n\033[1m══ summary ══\033[0m\n  passed: \033[32m%d\033[0m   failed: \033[31m%d\033[0m\n' "$PASS" "$FAIL"
printf '  usage (API-eq, NOT a subscription charge): $%s   wall: %ss\n' "${TOTAL:-?}" "$((E1 + E2 + ${E3:-0}))"
if [ "$FAIL" -gt 0 ]; then
  printf '\n\033[31mfailures:\033[0m\n'; for f in "${FAILED[@]}"; do printf '  • %s\n' "$f"; done
  exit 1
fi
printf '\n\033[32mall green — against a real model\033[0m\n'
