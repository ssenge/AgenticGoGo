#!/usr/bin/env bash
# REAL harness for the Rust-API samples: provision a scratch project with a genuine, small,
# objectively-checkable task, write the REAL judges the samples name, and (optionally) drive
# `examples/workflow.rs` / `examples/selfimprove.rs` against it with a REAL model.
#
# scripts/e2e_real.sh proves the YAML loop against a real agent. This proves the RUST DRIVER path:
# `Agg::open` → `step()` → `judge()` → `gate()` → `block()`, with the driver being the shipped
# sample itself (`target/debug/examples/*`), not a copy of it — that is why `main` was split into a
# thin `main()` + `drive(&agg, cycles)`: the cycle count and the project dir come from argv/env, so
# a harness can bound a run without editing the sample it is supposed to be exercising.
#
# NOTHING HERE IS FAKED. The `claude`/`codex` on PATH are PASSTHROUGH WRAPPERS (record argv + the
# phase agg had published, then `exec` the real binary) — the same trick e2e_real.sh uses to observe
# a run without stubbing it. The judges run real python and real measurements. The task is real:
# the seeded limiter has a genuine bug and a genuinely missing function, and two real tests fail
# until an agent actually fixes them.
#
#   ./scripts/samples_real.sh                       # scaffold only (default) — no tokens spent
#   ./scripts/samples_real.sh --check               # scaffold + run every judge on the SEED (free)
#   ./scripts/samples_real.sh --run workflow --cycles 2
#   ./scripts/samples_real.sh --run selfimprove --cycles 2 --dir /tmp/mine
#   ./scripts/samples_real.sh --run workflow --heavy claude-sonnet-5 --grind claude-haiku-4-5-20251001
#
# THE TASK (the whole point — small, real, verifiable by a script, minutes not hours):
#   src/ratelimit/limiter.py is a token bucket with TWO defects the tests pin down:
#     1. `_refill` truncates elapsed time with int(), so sub-second refills never happen;
#     2. `allow_n(n)` — the all-or-nothing multi-token take the spec requires — is missing entirely.
#   tests/test_limiter.py is deterministic (injected clock, no sleeps): 2 of 6 tests fail on the
#   seed. "Done" is not a model's opinion; it is `python3 -m unittest` exiting 0.
#
# COST: the owner is on a subscription. The `$` figures agg prints are API-EQUIVALENT LIST PRICE,
# not a charge. Bound the run with --cycles anyway; 2 is enough to prove a loop loops.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="${AGG_SAMPLE_DIR:-${TMPDIR:-/tmp}/agg-samples-real}"
RUN=""; CYCLES="${AGG_SAMPLE_CYCLES:-2}"; CHECK=""; FRESH=1; UNBLOCK=1
HEAVY="${AGG_SAMPLE_HEAVY_MODEL:-}"; GRIND="${AGG_SAMPLE_GRIND_MODEL:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    --dir)      DIR="$2"; shift 2 ;;
    --run)      RUN="$2"; shift 2 ;;
    --cycles)   CYCLES="$2"; shift 2 ;;
    --heavy)    HEAVY="$2"; shift 2 ;;
    --grind)    GRIND="$2"; shift 2 ;;
    --check)    CHECK=1; shift ;;
    --keep)     FRESH=0; shift ;;          # scaffold over an existing dir (keeps agent work)
    --no-unblock) UNBLOCK=0; shift ;;      # do NOT auto-release agg.block() — a human will
    -h|--help)  sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done
case "${RUN:-}" in ""|workflow|selfimprove|yaml) ;; *) echo "--run takes workflow|selfimprove|yaml" >&2; exit 2 ;; esac
# A lap of examples/workflow.yaml is at most 3+2+1+8+1+1 = 16 dispatches, so this is the session
# budget that lets `--cycles N` mean "N full laps" on the YAML path too. `agg run` has no notion of
# a lap — the walk wraps forever — so a ceiling is the only way to bound it.
MAX_SESSIONS=$(( CYCLES * 16 ))

sec() { printf '\n\033[1m── %s\033[0m\n' "$*"; }
ok()  { printf '  \033[32m✔\033[0m %s\n' "$1"; }
bad() { printf '  \033[31m✘ %s\033[0m\n' "$1"; }

# ═══════════════════════════════════════════════════════════════════════════════════════════
sec "build (the samples ARE the drivers — they must compile before they can drive)"
( cd "$ROOT" && cargo build --quiet --examples ) || { bad "cargo build --examples"; exit 1; }
ok "target/debug/examples/{workflow,selfimprove} + target/debug/agg"

REAL_CLAUDE="$(command -v claude || true)"
REAL_CODEX="$(command -v codex || true)"
[ -x "$REAL_CLAUDE" ] || { bad "claude not on PATH"; exit 1; }
[ -x "$REAL_CODEX" ]  || printf '  \033[33m! codex not on PATH — the Codex steps will fail\033[0m\n'

# ═══════════════════════════════════════════════════════════════════════════════════════════
sec "scaffold: a real project with a real, failing task"
[ "$FRESH" = 1 ] && rm -rf "$DIR"
mkdir -p "$DIR"/{bin,src/ratelimit,tests,internal,agg/judges,agg/state/wiki}

# ── the library under test: a token bucket with two REAL defects ────────────────────────────
cat > "$DIR/src/ratelimit/__init__.py" <<'PY'
"""A tiny rate-limiting library. The public surface is TokenBucket."""

from .limiter import TokenBucket

__all__ = ["TokenBucket"]
PY

cat > "$DIR/src/ratelimit/limiter.py" <<'PY'
"""Token-bucket rate limiting.

The contract lives in internal/SPEC.md and is pinned by tests/test_limiter.py.
"""

import time


class TokenBucket:
    """Allow up to `capacity` requests in a burst, refilling `rate` tokens per second.

    The clock is injected so callers (and tests) can drive time deterministically.
    """

    def __init__(self, capacity, rate, clock=time.monotonic):
        self.capacity = float(capacity)
        self.rate = float(rate)
        self._clock = clock
        self._tokens = float(capacity)
        self._last = clock()

    def _refill(self):
        now = self._clock()
        elapsed = int(now - self._last)
        self._tokens = min(self.capacity, self._tokens + elapsed * self.rate)
        self._last = now

    def tokens(self):
        """Tokens available right now (refilling first)."""
        self._refill()
        return self._tokens

    def allow(self):
        """Take one token. True if it was there to take."""
        self._refill()
        if self._tokens >= 1.0:
            self._tokens -= 1.0
            return True
        return False
PY

# ── the spec: short, authoritative, and the thing a session can act on in minutes ───────────
cat > "$DIR/internal/SPEC.md" <<'MD'
# ratelimit — the contract

`TokenBucket(capacity, rate, clock=time.monotonic)`

1. **Burst.** A fresh bucket allows `capacity` calls to `allow()` in a row, then refuses.
2. **Refill is CONTINUOUS.** After `dt` seconds, `dt * rate` tokens are back — fractional `dt`
   included. Half a second at 10/s returns 5 tokens. Never more than `capacity`.
3. **`allow_n(n)` is ALL-OR-NOTHING.** It takes `n` tokens and returns True, or takes NOTHING and
   returns False. A partial take is a bug: it would let a caller drain the bucket while being
   told it was refused.
4. **`tokens()`** reports what is available now (refilling first).
5. No sleeps, no threads, no global clock reads outside the injected `clock`.

Two of these are NOT implemented today. `python3 -m unittest discover -s tests` says which.
MD

cat > "$DIR/internal/ROADMAP.md" <<'MD'
# ROADMAP

Each item is completable in one session and verifiable by `agg/judges/tests.sh`.

- [ ] **continuous-refill** — `_refill` truncates elapsed time with `int()`, so anything under a
      second refills nothing. Make refill continuous (see internal/SPEC.md §2).
- [ ] **allow-n** — add the all-or-nothing `allow_n(n)` from internal/SPEC.md §3.
- [ ] **sliding-window** — a second limiter, `SlidingWindow(limit, window)`, sharing the injected
      clock. Only after the two above are green.
- [ ] **docs** — a README with a worked example of each limiter.
MD

# ── the tests: deterministic (injected clock, zero sleeps), 2 of 6 RED on the seed ──────────
cat > "$DIR/tests/test_limiter.py" <<'PY'
"""The executable contract. A fake clock keeps every assertion deterministic."""

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

from ratelimit import TokenBucket  # noqa: E402


class FakeClock:
    """A clock the test moves by hand. No sleeps anywhere in this suite."""

    def __init__(self):
        self.t = 0.0

    def __call__(self):
        return self.t

    def advance(self, dt):
        self.t += dt


class TestTokenBucket(unittest.TestCase):
    def setUp(self):
        self.clock = FakeClock()

    def bucket(self, capacity=5, rate=10):
        return TokenBucket(capacity, rate, clock=self.clock)

    def test_burst_up_to_capacity(self):
        b = self.bucket()
        self.assertTrue(all(b.allow() for _ in range(5)))
        self.assertFalse(b.allow())

    def test_refills_continuously_within_a_second(self):
        """SPEC 2 — half a second at 10/s is 5 tokens, not 0."""
        b = self.bucket()
        for _ in range(5):
            b.allow()
        self.clock.advance(0.5)
        self.assertTrue(b.allow())
        self.assertAlmostEqual(b.tokens(), 4.0, places=6)

    def test_refill_is_capped_at_capacity(self):
        b = self.bucket()
        for _ in range(5):
            b.allow()
        self.clock.advance(100.0)
        self.assertAlmostEqual(b.tokens(), 5.0, places=6)

    def test_allow_n_takes_all_or_nothing(self):
        """SPEC 3 — a refused allow_n must not have consumed anything."""
        b = self.bucket()
        self.assertTrue(b.allow_n(3))
        self.assertAlmostEqual(b.tokens(), 2.0, places=6)
        self.assertFalse(b.allow_n(3))
        self.assertAlmostEqual(b.tokens(), 2.0, places=6)

    def test_allow_n_of_one_matches_allow(self):
        b = self.bucket(capacity=2, rate=1)
        self.assertTrue(b.allow_n(1))
        self.assertTrue(b.allow_n(1))
        self.assertFalse(b.allow_n(1))

    def test_clock_is_injected_not_global(self):
        b = self.bucket()
        self.assertIs(b._clock, self.clock)


if __name__ == "__main__":
    unittest.main()
PY

# ── the worker's forward note (the Rust path reads agg/state/ the same way) ─────────────────
cat > "$DIR/agg/state/STATE.md" <<'MD'
# STATE

First session. The library is `src/ratelimit/`, the contract is `internal/SPEC.md`, the executable
contract is `tests/test_limiter.py` (run: `python3 -m unittest discover -s tests`). Two tests are
RED on purpose — a truncating refill and a missing `allow_n`. Fix the LIBRARY, never the tests.

Rewrite this note each session with what the next session needs and does not already know.
MD

cat > "$DIR/.gitignore" <<'EOF'
__pycache__/
*.pyc
agg/state/
agg/private/
bin/
claude_args.txt
trace.txt
*.log
EOF

# ── passthrough wrappers: observe the real run without faking any part of it ────────────────
cat > "$DIR/rec" <<'EOF'
#!/bin/sh
printf '%s=%s\n' "$1" "$(sed -n 's/.*"phase":"\([a-z]*\)".*/\1/p' agg/private/state.json 2>/dev/null)" >> trace.txt
EOF
for pair in "claude:$REAL_CLAUDE" "codex:$REAL_CODEX"; do
  name="${pair%%:*}"; real="${pair#*:}"
  [ -n "$real" ] || continue
  cat > "$DIR/bin/$name" <<EOF
#!/bin/sh
# PASSTHROUGH — record argv + the phase agg had published, then run the REAL $name. Nothing faked.
for a in "\$@"; do [ "\$a" = "--version" ] && exec "$real" "\$@"; done
printf '%s %s\n' "$name" "\$*" >> ${name}_args.txt
sh ./rec RUN
exec "$real" "\$@"
EOF
  chmod +x "$DIR/bin/$name"
done
chmod +x "$DIR/rec"
ok "src/ratelimit + tests + internal/{SPEC,ROADMAP}.md + passthrough wrappers"

# ═══════════════════════════════════════════════════════════════════════════════════════════
sec "the judges the samples name — real commands, real exit codes"
J="$DIR/agg/judges"

# A script judge is read from its STDOUT as verdict JSON (core/judge.rs:3). Each script below runs a
# REAL command, keeps its REAL exit code in `$rc`, and reports that — the JSON is the wire format,
# never a substitute for actually running the thing.

cat > "$J/build.sh" <<'EOF'
#!/bin/sh
# BUILD — byte-compile every module. Non-zero means a syntax or import-time error.
out=$(python3 -m compileall -q src tests 2>&1); rc=$?
if [ "$rc" = 0 ]; then
  echo '{"met":true,"value":1,"max":1,"target":1,"rationale":"all modules compile"}'
else
  printf '{"met":false,"value":0,"max":1,"target":1,"rationale":"compile failed: %s"}\n' \
    "$(printf '%s' "$out" | tr -d '"\\' | tr '\n' ' ' | cut -c1-200)"
fi
exit "$rc"    # compileall's real status, carried out of the judge
EOF

cat > "$J/lint.sh" <<'EOF'
#!/bin/sh
# LINT — a real (small) linter over the library: line length, tabs, bare except, star imports and
# library-level print(). stdlib only, so it needs no toolchain the agent might not have.
python3 - <<'PY'
import json, pathlib, re, sys

bad = []
for p in sorted(pathlib.Path("src").rglob("*.py")):
    for i, line in enumerate(p.read_text().splitlines(), 1):
        if len(line) > 100:               bad.append(f"{p}:{i} line >100 chars")
        if "\t" in line:                  bad.append(f"{p}:{i} tab")
        if re.match(r"\s*except\s*:", line):   bad.append(f"{p}:{i} bare except")
        if re.match(r"\s*from .* import \*", line): bad.append(f"{p}:{i} star import")
        if re.match(r"\s*print\(", line): bad.append(f"{p}:{i} print() in a library")
print(json.dumps({"met": not bad, "value": 0 if bad else 1, "max": 1, "target": 1,
                  "rationale": "clean" if not bad else f"{len(bad)} violations: " + "; ".join(bad[:5])}))
sys.exit(0 if not bad else 1)
PY
EOF

cat > "$J/tests.sh" <<'EOF'
#!/bin/sh
# TESTS — the real suite. `met` is the REAL exit code of unittest; `value` is the number of tests it
# collected, which is what an anti-shrink judge (examples/selfimprove.rs `no_shrink`) compares.
out=$(python3 -m unittest discover -s tests -p 'test_*.py' 2>&1); rc=$?
n=$(printf '%s' "$out" | sed -n 's/^Ran \([0-9]*\) test.*/\1/p' | tail -1); n=${n:-0}
tail=$(printf '%s' "$out" | tr -d '"\\' | tr '\n' ' ' | tail -c 200)
if [ "$rc" = 0 ]; then
  printf '{"met":true,"value":%s,"max":%s,"target":%s,"rationale":"%s tests pass"}\n' "$n" "$n" "$n" "$n"
else
  printf '{"met":false,"value":%s,"max":%s,"target":%s,"rationale":"suite RED: %s"}\n' "$n" "$n" "$n" "$tail"
fi
exit "$rc"    # the judge's own exit status IS unittest's — the JSON is the wire format, not a mask
EOF

cat > "$J/loadtest.sh" <<'EOF'
#!/bin/sh
# LOAD — a fast STAND-IN for a 40-minute soak, but it genuinely measures: 200k real allow() calls
# against a synthetic clock, per-op latency sampled with perf_counter_ns, p50/p99 in ms and
# throughput written to $AGG_JUDGE_SCRATCH/bench.json — which the `p99_ok` judge (YAML) and
# examples/workflow.rs's native p99 closure (Rust) both read.
#
# ⚠ NOT agg/state/bench.json. Since §2.5 the project tree is READ-ONLY to a judge — a judge that can
# write the tree it grades can make the code pass — so the measure/threshold handoff goes through the
# per-session judge scratch, which is the one place judges may write and is shared between them.
python3 - <<'PY'
import json, os, pathlib, sys, time
sys.path.insert(0, "src")
SCRATCH = pathlib.Path(os.environ.get("AGG_JUDGE_SCRATCH", "agg/state"))

OPS, SAMPLE, FLOOR = 200_000, 20_000, 50_000.0   # ops, latency samples, min ops/sec to pass
try:
    from ratelimit import TokenBucket
    t = [0.0]
    b = TokenBucket(1_000, 1_000_000, clock=lambda: t[0])
    lat = []
    t0 = time.perf_counter()
    for i in range(OPS):
        t[0] += 1e-5                      # synthetic clock: exercises the refill path every call
        if i < SAMPLE:
            s = time.perf_counter_ns(); b.allow(); lat.append(time.perf_counter_ns() - s)
        else:
            b.allow()
    wall = time.perf_counter() - t0
    lat.sort()
    p = lambda q: lat[min(len(lat) - 1, int(len(lat) * q))] / 1e6
    ops = OPS / wall if wall else 0.0
    res = {"ops": OPS, "wall_s": round(wall, 4), "ops_per_sec": round(ops, 1),
           "p50_ms": round(p(0.50), 6), "p99_ms": round(p(0.99), 6)}
    SCRATCH.mkdir(parents=True, exist_ok=True)
    (SCRATCH / "bench.json").write_text(json.dumps(res, indent=2) + "\n")
    met = ops >= FLOOR
    print(json.dumps({"met": met, "value": res["ops_per_sec"], "max": None, "target": FLOOR,
                      "rationale": f"{res['ops_per_sec']} ops/s, p99 {res['p99_ms']}ms over {OPS} real calls"}))
    sys.exit(0 if met else 1)
except Exception as e:                    # a broken limiter is a FAILED load test, not a crashed judge
    print(json.dumps({"met": False, "value": 0, "max": None, "target": FLOOR,
                      "rationale": f"load run raised {type(e).__name__}: {e}"}))
    sys.exit(1)
PY
EOF

cat > "$J/e2e.sh" <<'EOF'
#!/bin/sh
# E2E — the whole thing through the PUBLIC surface: import the installed-shape package, drive a real
# scenario (burst → partial refill → all-or-nothing take → cap), and fail loudly on the first lie.
# Slower and broader than tests.sh on purpose: this is the judge the samples put behind `&&`.
python3 -m compileall -q src tests >/dev/null 2>&1 || {
  echo '{"met":false,"value":0,"max":1,"target":1,"rationale":"does not compile"}'; exit 1; }
python3 - <<'PY'
import json, sys
sys.path.insert(0, "src")
try:
    from ratelimit import TokenBucket
    t = [0.0]
    b = TokenBucket(5, 10, clock=lambda: t[0])
    assert all(b.allow() for _ in range(5)), "burst of capacity refused"
    assert not b.allow(), "allowed past capacity"
    t[0] += 0.5
    assert abs(b.tokens() - 5.0) < 1e-6, f"0.5s at 10/s should refill 5, got {b.tokens()}"
    assert b.allow_n(4), "all-or-nothing take of 4 refused with 5 available"
    assert not b.allow_n(4), "took 4 twice from a bucket of 5"
    assert abs(b.tokens() - 1.0) < 1e-6, f"a refused allow_n consumed tokens: {b.tokens()}"
    t[0] += 1000.0
    assert abs(b.tokens() - 5.0) < 1e-6, "refill exceeded capacity"
    print(json.dumps({"met": True, "value": 1, "max": 1, "target": 1,
                      "rationale": "burst, sub-second refill, atomic allow_n and the cap all hold"}))
except Exception as e:
    print(json.dumps({"met": False, "value": 0, "max": 1, "target": 1,
                      "rationale": f"{type(e).__name__}: {e}"}))
    sys.exit(1)
PY
EOF

# ALIASES. The two sample paths NAME judges differently, and only one of them can choose:
#
#   Rust  — `Judge::script("tests_pass", "agg/judges/tests.sh")` maps a NAME to any PATH it likes.
#   YAML  — resolves by NAME ONLY: `tests_pass` is `agg/judges/tests_pass.{sh,md}` or it is a
#           STARTUP ERROR. That is `agg doctor`'s "no judge named `tests_pass`".
#
# So the YAML sample cannot run against a project that only has `tests.sh`, however good that script
# is. One implementation per check, as many names as the samples ask for — never a second copy,
# because two implementations of one check drift and then the two samples grade differently.
# p99_ok is NOT an alias: it is workflow.yaml's gap (1) made real — the Rust sample's native closure
# reads one field out of the benchmark, and the YAML path has no native judges, so the same threshold
# has to be a script. It reads what load_ok measured; it never measures again.
cat > "$J/p99_ok.sh" <<'EOF'
#!/bin/sh
# P99 — a THRESHOLD over load_ok's measurement, not a second measurement. Reads bench.json out of
# the shared per-session judge scratch (the one place a judge may write since §2.5).
python3 - <<'PY'
import json, os, pathlib, sys
CEIL = 0.5   # ms
b = pathlib.Path(os.environ.get("AGG_JUDGE_SCRATCH", "agg/state")) / "bench.json"
if not b.exists():
    # NOT met, and NOT a lie about latency: say which judge has to run first.
    print(json.dumps({"met": False, "value": None, "max": None, "target": CEIL,
                      "rationale": "no bench.json — load_ok has not run this session"}))
    sys.exit(1)
p99 = json.loads(b.read_text())["p99_ms"]
print(json.dumps({"met": p99 <= CEIL, "value": p99, "max": None, "target": CEIL,
                  "rationale": f"p99 {p99}ms against a {CEIL}ms ceiling"}))
sys.exit(0 if p99 <= CEIL else 1)
PY
EOF

for a in "build_ok.sh:build.sh" "lint_clean.sh:lint.sh" "cargo_test.sh:tests.sh" \
         "builds.sh:build.sh" "tests_pass.sh:tests.sh" "load_ok.sh:loadtest.sh"; do
  cat > "$J/${a%%:*}" <<EOF
#!/bin/sh
# ALIAS — a sample names this judge; against THIS project the same check is ${a#*:}.
exec "\$(dirname "\$0")/${a#*:}"
EOF
done
chmod +x "$J"/*.sh

# ── the rubric judges: graded by the REAL ruler, `inputs:` is the evidence it sees ──────────
cat > "$J/survey_good.md" <<'MD'
---
inputs: ["agg/state/wiki/survey.md", "internal/SPEC.md"]
---
You are grading a technical SURVEY of rate-limiting algorithms written for this project: a
single-process Python token bucket with an injected clock (see the SPEC in the artifacts).

Score 0-100. Start at 0 and award only what is evidenced in the artifact:

- 30 — at least three named algorithms (e.g. token bucket, leaky bucket, fixed/sliding window,
  GCRA), each with its actual MECHANISM described, not just its name.
- 25 — real trade-offs: burst tolerance, memory per key, behaviour under a distributed deployment,
  and clock sensitivity. A list of adjectives scores 0 here; a comparison scores full.
- 20 — a clear RECOMMENDATION for THIS project, with the reason tied to the SPEC's constraints.
- 15 — sources: links or citations a reader could follow.
- 10 — brevity and structure: skimmable, under roughly two pages, no filler.

Deduct everything for a claim contradicted by the SPEC. A missing or empty survey scores 0.

Report `value` = the score, `max` = 100, `target` = 85, and `met` = true only if the score is 85
or above.
MD

cat > "$J/spec_sound.md" <<'MD'
---
inputs: ["agg/state/wiki/spec.md", "agg/state/wiki/survey.md", "internal/SPEC.md", "tests/test_limiter.py"]
---
You are grading an implementation SPEC — a document another engineer must be able to build from
with none of the author's context. The project's authoritative contract (`internal/SPEC.md`) and
its executable contract (`tests/test_limiter.py`) are in the artifacts; where the spec disagrees
with the TESTS, the tests are right and the spec is wrong.

Score 0-100:

- 35 — the exact API is pinned: names, parameters, return types, and the ALL-OR-NOTHING semantics
  of a multi-token take. Refill must be specified as continuous (fractional seconds), because that
  is what the tests assert.
- 25 — it is implementable as written: no step requires a decision the document does not make.
- 20 — it says explicitly what the survey MISSED or got wrong (the step that produced it was told
  to assume the survey is incomplete).
- 10 — edge cases: capacity cap, n larger than capacity, non-monotonic or repeated clock reads.
- 10 — it names how the work will be verified (the real test command).

`met` = true only if the score is 85 or above, and NEVER if the spec contradicts the tests.
Report `value` = score, `max` = 100, `target` = 85.
MD

cat > "$J/design_sound.md" <<'MD'
---
inputs: ["agg/state/wiki/next.md", "internal/ROADMAP.md", "internal/SPEC.md", "src/ratelimit/limiter.py"]
---
You are grading a DESIGN NOTE that selects the next roadmap item and states the contract for it.
The roadmap, the project spec, and the current source are in the artifacts.

Score 0-100:

- 30 — it picks exactly ONE unstarted roadmap item, completable in a single session, and says why
  that one (value and dependency order — the sliding window is blocked on the two fixes above it).
- 30 — the contract is grounded in code that actually exists: it names the real file, the real
  function, and the real defect it is addressing. A claim about code not present in the artifacts
  scores 0 for this section.
- 20 — it states how the change will be VERIFIED, with the real command.
- 20 — it is buildable without the author: no "and then handle the rest", no undefined terms.

`met` = true only if the score is 85 or above. Report `value` = score, `max` = 100, `target` = 85.
MD
ok "scripts: build/lint/tests/loadtest/e2e (+ build_ok/lint_clean/cargo_test aliases)"
ok "rubrics: survey_good/spec_sound/design_sound — graded by the real ruler"

# ── git: session isolation is mandatory, so the project must be a repo ──────────────────────
if [ ! -d "$DIR/.git" ]; then
  ( cd "$DIR" && git init -q -b main && git config user.email samples@agg && git config user.name samples \
      && git add -A && git commit -q -m "seed: token bucket with a truncating refill and no allow_n" )
fi
ok "git repo seeded on main ($(cd "$DIR" && git rev-parse --short HEAD))"
printf '  project: \033[1m%s\033[0m\n' "$DIR"

# ═══════════════════════════════════════════════════════════════════════════════════════════
if [ -n "$CHECK" ]; then
  sec "judge check — every judge, against the SEED (no model, no tokens)"
  # The seed MUST be green on build/lint/load and RED on tests/e2e. If that is not what the judges
  # say, the task is not the task and a real run would be judging the wrong thing.
  for j in build lint tests loadtest e2e; do
    out=$( cd "$DIR" && "./agg/judges/$j.sh" 2>&1 ); rc=$?
    met=$(printf '%s' "$out" | python3 -c 'import json,sys;print(json.loads(sys.stdin.read().strip().splitlines()[-1])["met"])' 2>/dev/null)
    printf '  %-9s exit=%s met=%-5s %s\n' "$j" "$rc" "${met:-?}" \
      "$(printf '%s' "$out" | tail -1 | cut -c1-120)"
  done
  case "$( cd "$DIR" && ./agg/judges/tests.sh | tail -1 )" in
    *'"met":false'*) ok "the seed is genuinely RED (an agent has real work to do)" ;;
    *) bad "the seed already passes — the task is not a task" ;;
  esac
fi

# ═══════════════════════════════════════════════════════════════════════════════════════════
if [ -z "$RUN" ]; then
  cat <<EOF

next (each spends REAL subscription usage):
  $0 --run workflow    --cycles $CYCLES --dir $DIR --keep
  $0 --run selfimprove --cycles $CYCLES --dir $DIR --keep
  $0 --run yaml        --cycles $CYCLES --dir $DIR --keep   # examples/workflow.yaml, verbatim
watch:   (cd $DIR && $ROOT/target/debug/agg dashboard)
release a block():     $ROOT/scripts/agg_unblock.sh $DIR
EOF
  exit 0
fi

sec "RUN — $RUN, $CYCLES cycles, against a real model"
LOG="$DIR/$RUN.log"
if [ "$RUN" = yaml ]; then
  # ⚠ THE COMMITTED FILE, BYTE FOR BYTE. The point of this mode is that the sample a user reads is
  # the sample that ran — an earlier "the YAML sample works" claim rested on a hand-edited variant
  # (cheaper models, shorter timeouts) that differed from the shipped file in ~68 lines, which is
  # not evidence about the shipped file. Do NOT add sed overrides here; if a knob makes the sample
  # unrunnable, fix the SAMPLE.
  cp "$ROOT/examples/workflow.yaml" "$DIR/agg/agg.yaml"
  ok "agg/agg.yaml ← examples/workflow.yaml (verbatim, $(wc -l < "$ROOT/examples/workflow.yaml" | tr -d ' ') lines)"
  BIN="$ROOT/target/debug/agg"
  printf '  driver: %s run --max-sessions %s  (the WALK, not a Rust driver)\n  log:    %s\n' "$BIN" "$MAX_SESSIONS" "$LOG"
else
  BIN="$ROOT/target/debug/examples/$RUN"
  printf '  driver: %s\n  log:    %s\n' "$BIN" "$LOG"
fi
[ -x "$BIN" ] || { bad "missing $BIN"; exit 1; }
printf '  \033[33mthis spends real subscription usage (the $ agg prints is API-equivalent list price).\033[0m\n'

# The samples call `agg.block(..)` and WAIT on the operator bus. Unattended, nobody answers — so
# release it from outside, which is exactly what the bus is for (`agg send resume`).
if [ "$UNBLOCK" = 1 ]; then
  "$ROOT/scripts/agg_unblock.sh" "$DIR" --watch "$LOG" --answer "auto-approved by samples_real.sh" &
  UNBLOCK_PID=$!
  trap '[ -n "${UNBLOCK_PID:-}" ] && kill "$UNBLOCK_PID" 2>/dev/null' EXIT
  ok "auto-unblocker watching (pid $UNBLOCK_PID) — pass --no-unblock to answer block() yourself"
fi

T0=$(date +%s)
if [ "$RUN" = yaml ]; then
  ( cd "$DIR" && PATH="$DIR/bin:$PATH" "$BIN" run --max-sessions "$MAX_SESSIONS" ) > "$LOG" 2>&1
else
  ( cd "$DIR" && PATH="$DIR/bin:$PATH" \
      AGG_SAMPLE_HEAVY_MODEL="$HEAVY" AGG_SAMPLE_GRIND_MODEL="$GRIND" \
      "$BIN" . "$CYCLES" ) > "$LOG" 2>&1
fi
RC=$?
EL=$(( $(date +%s) - T0 ))

sec "what the real run did"
printf '  exit=%s  wall=%ss\n' "$RC" "$EL"
printf '  trace: %s\n' "$(tr '\n' ' ' < "$DIR/trace.txt" 2>/dev/null | cut -c1-200)"
python3 - "$DIR" <<'PY' 2>/dev/null || echo "  (no state.json — the run did not start)"
import json, sys
d = json.load(open(sys.argv[1] + "/agg/private/state.json"))
print(f"  sessions={d.get('session')}  phase={d.get('phase')}  "
      f"out-tok={d.get('tokens_spent')}  usage(API-eq)=${d.get('cost_spent')}")
PY
grep -c '^## session' "$DIR/agg/private/LOG.md" 2>/dev/null | sed 's/^/  memory entries: /'
( cd "$DIR" && ./agg/judges/tests.sh | tail -1 | sed 's/^/  tests judge: /' )
( cd "$DIR" && git log --oneline -n 8 | sed 's/^/  git: /' )
printf '\n  full log: %s\n' "$LOG"
exit $RC
