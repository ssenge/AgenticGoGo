#!/usr/bin/env bash
# REAL metric and score judges on the container tier: the REAL swe-metrics, in the reference judges'
# image (docker/swe-metrics), measuring a fixture with KNOWN complexity, duplication and
# vulnerabilities — behind a network allow-list that holds only the worker's endpoint, with a
# swe-metrics stub first on the HOST's PATH that fails (and leaves a mark) if anything calls it.
#
# scripts/e2e.sh and `cargo test` (tests/metrics_container.rs) cover the same mechanisms with a FAKE
# swe-metrics. This one needs the reference image, which is large (~2 GB) and built from a local
# swe-metrics checkout, so it is run BY HAND:
#
#   docker/swe-metrics/build.sh ~/src/swe-metrics            # once → agg-swe-metrics
#   ./scripts/e2e_metrics_container_real.sh
#   SWE_METRICS_IMAGE=my-judges ./scripts/e2e_metrics_container_real.sh
#   KEEP=1 ./scripts/e2e_metrics_container_real.sh            # keep the workspace (under target/)
#
# No tokens: the worker is the fake-agent test image. Needs a running container engine (colima
# works). trivy's database is the host's (`trivy image --download-db-only`); without one, the check
# on the vulnerability count expects the fail-closed error instead.
#
# Exits 0 only if every check passed.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WS="$ROOT/target/agg-e2e-metrics-real.$$"
AGG="$ROOT/target/debug/agg"
ENGINE="$(command -v docker || command -v podman || true)"
IMAGE="${SWE_METRICS_IMAGE:-agg-swe-metrics}"

PASS=0; FAIL=0; declare -a FAILED=()
sec()  { printf '\n\033[1m── %s\033[0m\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf '  \033[32m✔\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); FAILED+=("$1"); printf '  \033[31m✘ %s\033[0m\n' "$1"; [ -n "${2:-}" ] && printf '      %s\n' "$2"; return 0; }
is()   { [ "$2" = "$3" ] && ok "$1" || bad "$1" "expected [$3], got [$2]"; }
has()  { grep -qF -- "$3" "$2" 2>/dev/null && ok "$1" || bad "$1" "'$3' not found in $2"; }

trap '[ -n "${KEEP:-}" ] || rm -rf "$WS"' EXIT
[ -n "$ENGINE" ] || { echo "no container engine (docker/podman) on PATH"; exit 1; }
"$ENGINE" version >/dev/null 2>&1 || { echo "the container engine is not answering (colima start?)"; exit 1; }
"$ENGINE" image inspect "$IMAGE" >/dev/null 2>&1 || {
  echo "no image $IMAGE — build it first: docker/swe-metrics/build.sh <your swe-metrics checkout>"; exit 1; }
mkdir -p "$WS"

printf '\033[1mAgenticGoGo — REAL swe-metrics judges in the judges'"'"' image\033[0m\nworkspace: %s\nimage: %s\n' "$WS" "$IMAGE"
( cd "$ROOT" && cargo build --quiet ) || { bad "cargo build"; exit 1; }
"$ENGINE" build -q -t agg-test-fake-agent:1 "$ROOT/tests/fixtures/container/fake-agent" > "$WS/build.log" 2>&1
is "the fake-agent worker image builds" "$?" "0"

sec "the fixture: cyclomatic max 5, one clone, a requirement with known vulnerabilities"
D="$WS/proj"; mkdir -p "$D/agg/judges" "$D/agg/state" "$WS/hostbin"
cat > "$WS/hostbin/swe-metrics" <<EOF
#!/bin/sh
touch "$WS/HOST_SWE_METRICS_CALLED"; echo "host swe-metrics called" >&2; exit 1
EOF
chmod +x "$WS/hostbin/swe-metrics"
cat > "$D/calc.py" <<'EOF'
"""A small calculator with one branchy function."""


def classify(n):
    """Classify a number."""
    if n < 0:
        return "negative"
    if n == 0:
        return "zero"
    if n < 10:
        return "small"
    if n < 100:
        return "medium"
    return "large"
EOF
cat > "$D/report_a.py" <<'EOF'
"""Report A."""


def render(rows):
    """Render rows as text."""
    lines = []
    for row in rows:
        name = row.get("name", "")
        value = row.get("value", 0)
        unit = row.get("unit", "")
        label = f"{name}: {value} {unit}".strip()
        lines.append(label.upper())
        lines.append("-" * len(label))
    total = sum(r.get("value", 0) for r in rows)
    lines.append(f"total: {total}")
    return "\n".join(lines)
EOF
sed 's/Report A/Report B/' "$D/report_a.py" > "$D/report_b.py"
printf 'requests==2.19.0\n' > "$D/requirements.txt"
cat > "$D/agg/judges/worked.sh" <<'EOF'
#!/bin/sh
[ -f src/work.txt ] && echo '{"met":true,"value":1,"max":1,"target":1}' || echo '{"met":false,"value":0,"max":1,"target":1}'
EOF
chmod +x "$D/agg/judges/worked.sh"
echo work > "$D/agg/state/STATE.md"
printf 'agg/state/\nagg/private/\n' > "$D/.gitignore"
cat > "$D/agg/agg.yaml" <<EOF
project: metrics-real
defaults: { model: fake, isolation: container, image: "agg-test-fake-agent:1" }
judge: { image: "$IMAGE" }
judges:
  cc_max:  { metric: { id: "complexity.cyclomatic", stat: "max", tool: "lizard" }, max: 10 }
  clones:  { metric: { id: "duplication.clones" }, max: 5 }
  vulns:   { metric: { id: "dependencies.vulnerabilities" }, max: 1000 }
  quality: { score: { definition: "swe-default", version: "2" }, min: 0 }
steps: { worker: {} }
sequence: { steps: [worker], done_if: "worked AND cc_max AND clones AND quality", invariants: [vulns] }
summary: { enabled: false }
isolation_policy: { network: { allow: ["api.anthropic.com"] } }
EOF
( cd "$D" && git init -q -b main && git config user.email t@t && git config user.name t && git add -A && git commit -qm init )
HPATH="$WS/hostbin:$PATH"

( cd "$D" && PATH="$HPATH" "$AGG" doctor --json > "$WS/doctor.json" 2>/dev/null )
is "doctor --json: the judges' image has swe-metrics" \
   "$(python3 -c "import json;d=json.load(open('$WS/doctor.json'));print('yes' if any('swe-metrics' in i['clis'] and i['clis']['swe-metrics'] is not None for i in d['container_images']) else 'no')")" "yes"

sec "agg run — every measurement in $IMAGE"
( cd "$D" && PATH="$HPATH" "$AGG" run --max-sessions 1 > "$WS/run.log" 2>&1 )
is "agg run reaches done_if (exit 0)" "$?" "0"
[ -e "$WS/HOST_SWE_METRICS_CALLED" ] && bad "swe-metrics never ran on the host" || ok "swe-metrics never ran on the host"
value() { python3 -c "
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1])]
r = [r for r in rows if r['judge'] == sys.argv[2] and r.get('session') == int(sys.argv[3])]
print(r[0].get('value') if r else 'none')" "$D/agg/private/verdicts.jsonl" "$1" "$2"; }
error_of() { python3 -c "
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1])]
print(([r for r in rows if r['judge'] == sys.argv[2]][0].get('error') or ''))" "$D/agg/private/verdicts.jsonl" "$1"; }
is  "the fixture's known complexity: cyclomatic max 5" "$(value cc_max 1)" "5.0"
is  "the fixture's known duplication: one clone"        "$(value clones 1)" "1.0"
# where trivy on this host keeps its database — what agg mounts read-only (src/metrics/contained.rs)
host_trivy_db() {
  local d="${TRIVY_CACHE_DIR:-}"
  if [ -z "$d" ]; then
    if [ "$(uname -s)" = Darwin ]; then d="$HOME/Library/Caches/trivy"; else d="${XDG_CACHE_HOME:-$HOME/.cache}/trivy"; fi
  fi
  echo "$d/db/trivy.db"
}
if [ -f "$(host_trivy_db)" ]; then
  v="$(value vulns 1)"
  if [ "$v" != none ] && python3 -c "import sys; sys.exit(0 if float('$v') >= 1 else 1)" 2>/dev/null; then
    ok "trivy found the requirement's vulnerabilities offline, from the host's read-only database ($v)"
  else
    bad "trivy found the requirement's vulnerabilities" "value: $v; error: $(error_of vulns)"
  fi
else
  case "$(error_of vulns)" in *skip-db-update*|*trivy*) ok "no host trivy database: the vulnerability judge fails closed, saying why" ;;
    *) bad "no host trivy database: the vulnerability judge fails closed" "error: $(error_of vulns)" ;; esac
fi
[ "$(value quality 1)" != none ] && ok "the score judge has a score ($(value quality 1))" || bad "the score judge has a score" "$(error_of quality)"
[ -f "$D/agg/private/metrics/incumbent/target.json" ] && ok "the incumbent's report is kept under agg/private/" || bad "the incumbent's report is kept"
ls "$D"/agg/private/evidence/*/1/cc_max/swe-metrics/target/report.json >/dev/null 2>&1 \
  && ok "the shared report is kept as evidence" || bad "the shared report is kept as evidence"
is  "the project tree is clean (swe-metrics wrote nothing into it)" "$(cd "$D" && git status --porcelain -- . ':!src' | wc -l | tr -d ' ')" "0"

printf '\n\033[1m══ summary ══\033[0m\n  passed: %d   failed: %d\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
  for f in "${FAILED[@]}"; do printf '  • %s\n' "$f"; done
  exit 1
fi
printf '\033[32mall green\033[0m  (agg-test-fake-agent:1 and %s are left in the engine)\n' "$IMAGE"
