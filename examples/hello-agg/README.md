# hello-agg — the smallest possible loop

The whole idea in four files: a worker does a task, a **judge** (any script that prints one line
of JSON `{"met": …}`) checks it, and the loop repeats until the judge says met. **The judge IS your
Definition of Done, made executable.**

`add.py` starts WRONG on purpose (prints 3) so you can watch the correction loop happen.

## Layout

Everything agg reads lives under `agg/` (committed); everything it writes lives under `agg/state/`
(gitignored). One config file, `agg/agg.yaml` — a judge is resolved by NAME from
`agg/judges/`.

```
hello-agg/
  add.py                    # the (broken) target the worker fixes  — at the project root
  agg/
    agg.yaml                # defaults / judge / steps / sequence — the whole config
    AGG_STATE.md            # the worker's standing instructions
    judges/
      prints_two.sh         # the judge — resolved by the NAME `prints_two` in `done_if`
```

## Run it
```bash
agg plan        # dry run: evaluate the judge once, print the scoreboard. No agent launched.
agg run         # drive the loop until done_if (`prints_two`) is met
```

`agg plan` prints:

```
Goals: 0/1   done_if: prints_two
  · prints_two         script     no   — add.py did not print 2
```

The judge rejects `3` → the worker edits `add.py` to `print(1 + 1)` → the judge sees `2` →
`met:true` → `done_if` fires → the loop stops. That's the entire model.

## How `done_if` works

`done_if` is your Definition of Done, written as a boolean over judge names. Here it is a single
judge, `prints_two`. A real project composes several — `done_if: "outputs_two AND tests_pass"` — so
the loop only stops when every clause holds. The judge NAME in `done_if` resolves to
`agg/judges/<name>.sh` (a script judge) or `agg/judges/<name>.md` (an LLM rubric judge); the file
extension decides.

## Run it on another agent

The judge is a shell script, so nothing here is Claude-specific — only the `agent:` and `model:`
keys in `agg/agg.yaml` are. Edit `defaults:` (and `judge:`) and re-run, or let
`agg init --agent <a>` write a correct config for you:

```yaml
# Claude Code (as shipped)
defaults:
  agent: "claude"
  model: "claude-haiku-4-5-20251001"
```
```yaml
# OpenAI Codex — omit `model:` entirely; Codex picks one that fits your account
defaults:
  agent: "codex"
```
```yaml
# GitHub Copilot CLI
defaults:
  agent: "copilot"
  model: "auto"
```

Then `agg doctor` (checks the agent is installed and can do what this config asks) and `agg run`.
The loop, the judge and the gate behave identically on all three.

## Files
- `add.py` — the (broken) target the worker fixes
- `agg/agg.yaml` — the config: one `worker` step, `done_if: prints_two`
- `agg/AGG_STATE.md` — the worker's standing instructions (read into every session)
- `agg/judges/prints_two.sh` — the judge; prints `{"met":…}` — a plain script, so it works on any agent

---

## Walked example: drive a project to "all tests pass"

The whole thing end to end on a tiny project — a Python lib with three unimplemented functions
and a failing test suite. `agg` keeps `RUN`ning fresh agents until `VERIFY` goes green, then stops.

**1. The project** (`calc.py` has stubs that raise `NotImplementedError`; `test_calc.py` tests them):

```python
# calc.py
def add(a, b):       raise NotImplementedError
def factorial(n):    raise NotImplementedError
def is_prime(n):     raise NotImplementedError
```

**2. A judge** (`VERIFY`) — `agg/judges/tests_pass.sh` runs the suite and prints a verdict:

```bash
#!/usr/bin/env bash
out="$(python3 -m pytest -q 2>&1)"
passed=$(printf '%s' "$out" | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+' || echo 0)
failed=$(printf '%s' "$out" | grep -oE '[0-9]+ failed' | grep -oE '[0-9]+' || echo 0)
total=$(( ${passed:-0} + ${failed:-0} ))
met=$([ "${failed:-0}" -eq 0 ] && [ "$total" -gt 0 ] && echo true || echo false)
printf '{"met":%s,"value":%s,"max":%s,"target":%s,"rationale":"%s/%s tests pass"}\n' \
  "$met" "${passed:-0}" "$total" "$total" "${passed:-0}" "$total"
```

The judge emits `value`/`max` so the dashboard shows partial progress (`18/28`), but `met` is what
decides. There is no `type:` field any more — a judge that emits a `value` is displayed as a count;
one that emits only `{"met":…}` is binary.

**3. `agg/agg.yaml`** — the config, with a Definition of Done and a wall-clock ceiling:

```yaml
project: "calc"
defaults:
  agent: "claude"                  # ⚠️ CLAUDE-SHAPED — see "Run it on another agent" above
  model: "claude-opus-4-8[1m]"
  state: "AGG_STATE.md"
judge:
  agent: "claude"
  model: "claude-haiku-4-5-20251001" # the cheap RULER model for any LLM judges
  timeout: 300
steps:
  worker: {}
sequence:
  steps:
    - "worker"
  limits: { tokens: 2000000 }      # output-token ceiling → over_budget. Works on ANY agent.
  done_if: "tests_pass"            # the Definition of Done: the suite is green
  abort_if: "over_budget OR wall_hours >= 0.5"   # give up after 30 min or the token ceiling
```

The three ceilings — tokens, cost, sessions — are unified under **`sequence.limits:`** now. A stray
top-level `budget:` (or the retired `budget:`/`cost:`/`max_sessions:` keys) is a hard error at startup
(so a spend ceiling can never silently become decorative). `limits.cost` is Claude-only; on
Codex/Copilot omit it and rely on `limits.tokens`.

**4. `agg/AGG_STATE.md`** — the standing instructions `INJECT`ed into *every* fresh session:

```
# Goal
Make all tests in test_calc.py pass.
calc.py has add(a,b), factorial(n), is_prime(n) stubbed with NotImplementedError.

# This session
1. Run `python3 -m pytest -q` to see what's failing.
2. Implement the failing function(s) in calc.py — real, correct implementations.
3. Re-run pytest to confirm. Update this file with the next task, commit, and exit.
```

The agent maintains this file forward across sessions (best-effort — agg warns if a session leaves
it untouched). Institutional memory (`AGG_MEMORY.md`, "what we tried and rejected") is written by
agg, never the worker, and lives under `agg/state/`.

**5. Run it:**

```bash
agg plan        # dry run: one VERIFY pass — shows "tests_pass  0/3 — loop would continue"
agg run         # the outer loop: RUN real agents until VERIFY passes, then GATE stops
agg dashboard   # (optional, second terminal) live colored TUI
```

What happens: a fresh agent reads the injected prompt, runs pytest, implements the functions,
re-runs pytest → green, exits. `VERIFY` flips the judge `0/3 → 3/3`; `GATE` sees `done_if` met →
the loop exits after one session. A one-line summary records what it did.

### Or let `/agg:new` write it for you

In a project you've already planned (a PRD, ROADMAP, get-shit-done `.planning/`, or a README),
let the skill do it. First install it (once per project, or `--user` for good):

```bash
agg skills install        # installs for the agent in agg.yaml — or --agent claude|codex|copilot
```

Then, inside your agent:

```
/agg-new                                  # Claude Code  (/agg:new if you installed the plugin)
/agg-new                                  # GitHub Copilot
$agg-new                                  # OpenAI Codex — it uses $, not /
```

Or just **ask** on any of them: "set up AgenticGoGo for this project".

It **translates** whatever plan exists into a `done_if` plus a judge per clause (it doesn't replicate
your spec tooling), asks only about genuine gaps, and writes an `agg.yaml` shaped for **your** agent —
no cost guard on Codex or Copilot, which cannot report one. Then exit the agent and `agg run`.
