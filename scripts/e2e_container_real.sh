#!/usr/bin/env bash
# REAL isolation e2e: Claude Code and/or Codex as workers INSIDE their reference images (docker/claude,
# docker/codex) — editing the bind-mounted project, reading a read-only context directory, blocked
# from a `write_deny` oracle, behind a network ALLOW-LIST that holds only the agent's own endpoint,
# judged by a script judge that runs in the image too. On macOS it also runs the HOST's `claude`
# under `isolation: sandbox` behind the same allow-list (Seatbelt + agg's proxy, no DNS).
#
# scripts/e2e.sh and `cargo test` cover the same mechanisms with a FAKE agent. This one spends real
# tokens and needs real credentials, so it is run BY HAND:
#
#   claude (container): ANTHROPIC_API_KEY, or CLAUDE_CODE_OAUTH_TOKEN (make one with
#           `claude setup-token`). A macOS interactive login lives in the Keychain, which no
#           container can read.
#   codex  (container): CODEX_API_KEY (or OPENAI_API_KEY), or a `codex login` (~/.codex/auth.json is
#           mounted read-only into the container).
#   claude-sandbox (macOS only): your normal `claude` login on this Mac — nothing to export.
#
#   ./scripts/e2e_container_real.sh                   # every target that has what it needs
#   ./scripts/e2e_container_real.sh claude            # just one: claude | codex | claude-sandbox
#   KEEP=1 ./scripts/e2e_container_real.sh            # keep the workspace (under target/)
#
# Needs a running container engine (docker; colima works) and network for the image build. The
# workspace lives under target/: a VM-backed engine (colima, lima) cannot see $TMPDIR, and agg
# refuses to start a container run whose project the engine cannot see.
#
# Exits 0 only if every check passed.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WS="$ROOT/target/agg-e2e-container-real.$$"
AGG="$ROOT/target/debug/agg"
ENGINE="$(command -v docker || command -v podman || true)"

PASS=0; FAIL=0; declare -a FAILED=()
sec()  { printf '\n\033[1m── %s\033[0m\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf '  \033[32m✔\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); FAILED+=("$1"); printf '  \033[31m✘ %s\033[0m\n' "$1"; [ -n "${2:-}" ] && printf '      %s\n' "$2"; return 0; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$3], got [$2]"; }
has()  { grep -qF -- "$3" "$2" 2>/dev/null && ok "$1" || bad "$1" "'$3' not found in $2"; }

trap '[ -n "${KEEP:-}" ] || rm -rf "$WS"' EXIT
mkdir -p "$WS"
[ -n "$ENGINE" ] || { echo "no container engine (docker/podman) on PATH"; exit 1; }
"$ENGINE" version >/dev/null 2>&1 || { echo "the container engine is not answering (colima start?)"; exit 1; }

has_cred() {
  case "$1" in
    claude) [ -n "${ANTHROPIC_API_KEY:-}${CLAUDE_CODE_OAUTH_TOKEN:-}" ] ;;
    codex)  [ -n "${CODEX_API_KEY:-}${OPENAI_API_KEY:-}" ] || [ -f "$HOME/.codex/auth.json" ] ;;
    claude-sandbox) [ "$(uname -s)" = Darwin ] && command -v claude >/dev/null 2>&1 ;;
  esac
}
# the agent's own endpoint(s) — the ONLY hosts on the allow-list
endpoints() {
  case "$1" in
    claude*) echo '"api.anthropic.com"' ;;
    codex) if [ -n "${CODEX_API_KEY:-}${OPENAI_API_KEY:-}" ]; then echo '"api.openai.com"'; else echo '"chatgpt.com", "auth.openai.com"'; fi ;;
  esac
}
# Claude Code's telemetry/update checks would be refused by the list anyway; this skips them.
export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
# the credential VALUES, so the run can prove none of them reached agg's files
secrets() { printf '%s\n' "${ANTHROPIC_API_KEY:-}" "${CLAUDE_CODE_OAUTH_TOKEN:-}" "${CODEX_API_KEY:-}" "${OPENAI_API_KEY:-}" | grep -v '^$' || true; }

AGENTS="${1:-all}"
[ "$AGENTS" = all ] && AGENTS="claude codex claude-sandbox"

printf '\033[1mAgenticGoGo — REAL isolation e2e (container · sandbox · egress)\033[0m\nworkspace: %s\n' "$WS"
printf '\033[33mthis spends real usage.\033[0m\n'
( cd "$ROOT" && cargo build --quiet ) || { bad "cargo build"; exit 1; }

run_agent() {
  local target=$1 agent=${1%-sandbox} d="$WS/$1" kb="$WS/$1-kb" tier=container image="agg-ref-${1%-sandbox}:e2e"
  [ "$target" = claude-sandbox ] && tier=sandbox
  sec "$target — $agent worker, isolation: $tier, behind an allow-list of its own endpoint"
  if ! has_cred "$target"; then
    printf '  \033[33m∼\033[0m skipped: %s has nothing to authenticate with here (see the header)\n' "$target"
    return
  fi
  local tierkeys
  if [ "$tier" = container ]; then
    "$ENGINE" build -q -t "$image" "$ROOT/docker/$agent" > "$WS/$target-build.log" 2>&1
    is "the reference image builds" "$?" "0"
    tierkeys="isolation: \"container\", image: \"$image\""
  else
    tierkeys='isolation: "sandbox"'
  fi

  local word="PERIWINKLE-$$"
  mkdir -p "$kb" "$d/agg/judges" "$d/agg/state" "$d/tests"
  printf '# Project knowledge\n\nThe secret word is: %s\n' "$word" > "$kb/index.md"
  printf 'orig\n' > "$d/tests/oracle.txt"
  printf 'agg/state/\nagg/private/\n' > "$d/.gitignore"
  local model=""; [ "$agent" = claude ] && model='model: "claude-haiku-4-5-20251001", '
  cat > "$d/agg/agg.yaml" <<EOF
project: "ctr-real-$agent"
defaults: { agent: "$agent", ${model}${tierkeys}, write_deny: ["tests/**"] }
judge: { agent: "$agent" }
steps:
  worker:
    prompt: "Read the index.md of the context directory named kb (listed under Context in your brief). Create the file answer.txt in the project root containing exactly the secret word it names, and nothing else. Then run this exact command once and leave its output where it writes it: curl -sS -m 10 -o /dev/null -w '%{http_code}' https://example.com > egress.txt 2>&1 || echo FAILED >> egress.txt . Do not change anything under tests/."
sequence: { steps: ["worker"], done_if: "answered", limits: { sessions: 3 } }
summary: { enabled: false }
context:
  - { path: "$kb", as: "kb", mode: "ro" }
isolation_policy:
  network: { allow: [$(endpoints "$target")] }
  pass_env: ["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"]
EOF
  cat > "$d/agg/judges/answered.sh" <<EOF
#!/bin/sh
# runs INSIDE the image too (the run's strongest tier)
if [ "\$(tr -d '[:space:]' < answer.txt 2>/dev/null)" = "$word" ]; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"the secret word from the context"}'
else
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"answer.txt does not hold the word yet"}'
fi
EOF
  chmod +x "$d/agg/judges/answered.sh"
  printf 'answer the question in the brief\n' > "$d/agg/state/STATE.md"
  ( cd "$d" && git init -q -b main && git config user.email t@t && git config user.name t && git add -A && git commit -qm init )

  # logs live OUTSIDE the project: agg runs every session on a clean git tree.
  local log="$WS/$target-run.log"
  ( cd "$d" && "$AGG" doctor --json > "$WS/$target-doctor.json" 2>/dev/null )
  if [ "$tier" = container ]; then
    is "doctor --json: the image has the $agent CLI" \
       "$(python3 -c "import json;d=json.load(open('$WS/$target-doctor.json'));print('yes' if any(i['clis'].get('$agent') for i in d['container_images']) else 'no')")" "yes"
  fi

  ( cd "$d" && "$AGG" run > "$log" 2>&1 )
  is   "agg run reaches done_if (exit 0)"               "$?" "0"
  is   "the worker wrote the word it read from /context" "$(tr -d '[:space:]' < "$d/answer.txt" 2>/dev/null)" "$word"
  is   "the context is unchanged"                        "$(cat "$kb/index.md")" "$(printf '# Project knowledge\n\nThe secret word is: %s' "$word")"
  is   "the write_deny oracle is unchanged"              "$(cat "$d/tests/oracle.txt")" "orig"
  if [ "$tier" = container ]; then
    has "the brief listed the context at /context/kb"   "$d/agg/private/INSTRUCTIONS.md" "/context/kb"
  else
    has "the brief listed the context"                   "$d/agg/private/INSTRUCTIONS.md" "## Context (read-only)"
  fi
  # A-7's acceptance: the agent worked (above) AND example.com was unreachable from its shell
  if [ -f "$d/egress.txt" ]; then
    if grep -q '^200' "$d/egress.txt"; then bad "curl example.com from the worker was REFUSED" "egress.txt: $(cat "$d/egress.txt")"; else ok "curl example.com from the worker was refused ($(tr '\n' ' ' < "$d/egress.txt"))"; fi
    has "…by agg's proxy, which logged it"               "$log" "[egress] DENIED"
  else
    printf '  \033[33m∼\033[0m the agent did not run the curl probe (egress.txt missing) — nothing to assert about it\n'
  fi
  local leaked=0 s
  while IFS= read -r s; do
    [ -n "$s" ] && grep -rqF -- "$s" "$d/agg/private" "$log" 2>/dev/null && leaked=1
  done < <(secrets)
  is   "no credential value in agg/private or the run log" "$leaked" "0"
}

for a in $AGENTS; do run_agent "$a"; done

printf '\n\033[1m══ summary ══\033[0m\n  passed: %d   failed: %d\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
  for f in "${FAILED[@]}"; do printf '  • %s\n' "$f"; done
  exit 1
fi
printf '\033[32mall green\033[0m  (images agg-ref-*:e2e are left in the engine; remove them with `%s rmi`)\n' "$(basename "$ENGINE")"
