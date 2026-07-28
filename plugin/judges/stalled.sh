#!/usr/bin/env bash
# Standard library judge: STALL detector (§5.9). Its input is agg/private/verdicts.jsonl — the
# AGG-OWNED half of the runtime state, unwritable by a confined worker. That is the whole point:
# this judge decides "is the loop making progress", projects wire it to abort_if, so a worker able
# to append forged `merged` rows could end its own run.
#
#   met:true when, across the last K=3 MERGED steps: no binary judge changed `met`, AND no
#   numeric judge changed `value`. Judges with no row in the window are ignored; `stalled`
#   ignores its OWN rows. Fewer than K qualifying merged steps ⇒ met:false.
#
# K is hardcoded (library judges are parameterless — §5.1); a project overrides K by shadowing
# this file in agg/judges/stalled.sh. Only `merged` rows count — a rolled-back step's churn was
# undone and must not read as progress-or-stall.
set -u
DIR="${AGG_PROJECT_DIR:-.}"
LOG="$DIR/agg/private/verdicts.jsonl"

python3 - "$LOG" <<'PY'
import json, sys
K = 3
path = sys.argv[1]
try:
    lines = open(path).read().splitlines()
except OSError:
    print('{"met":false,"target":1,"rationale":"no verdicts.jsonl yet — nothing to stall on"}')
    sys.exit(0)

# ordered list of merged steps, each a dict judge -> (met, value); skip our own rows.
steps = []           # list of ((session, step), {judge: (met, value_or_None)})
index = {}
for ln in lines:
    ln = ln.strip()
    if not ln:
        continue
    try:
        r = json.loads(ln)
    except ValueError:
        continue
    if r.get("outcome") != "merged":
        continue
    if r.get("judge") == "stalled":
        continue
    key = (r.get("session"), r.get("step"))
    if key not in index:
        index[key] = len(steps)
        steps.append((key, {}))
    steps[index[key]][1][r["judge"]] = (bool(r.get("met")), r.get("value", None))

window = [s for (_, s) in steps][-K:]
if len(window) < K:
    print('{"met":false,"target":1,"rationale":"fewer than %d merged steps — not enough history to call a stall"}' % K)
    sys.exit(0)

# for every judge seen in the window, is it flat across the rows where it appears?
judges = set()
for s in window:
    judges.update(s.keys())

changed = []
for j in sorted(judges):
    seq = [s[j] for s in window if j in s]
    if len(seq) < 2:
        continue  # only one row in the window → can't have changed
    numeric = any(v is not None for (_, v) in seq)
    if numeric:
        vals = [v for (_, v) in seq if v is not None]
        if len(set(vals)) > 1:
            changed.append(j)
    else:
        mets = [m for (m, _) in seq]
        if len(set(mets)) > 1:
            changed.append(j)

if changed:
    print(json.dumps({"met": False, "target": 1,
        "rationale": "not stalled — moved in last %d steps: %s" % (K, ", ".join(changed))}))
else:
    print(json.dumps({"met": True, "target": 1,
        "rationale": "STALLED — no judge moved across the last %d merged steps" % K}))
PY
