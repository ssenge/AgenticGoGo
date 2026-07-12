# hello-agg — the smallest possible loop

The whole idea in five files: a worker does a task, a **judge** (any script that prints one
line of JSON `{"met": …}`) checks it, and the loop repeats until the judge says met.

`add.py` starts WRONG on purpose (prints 3) so you can watch the correction loop happen.

## Run it
```bash
cp AGG_RESUME.md.template AGG_RESUME.md   # the worker's standing instruction
chmod +x check.sh
agg run
```

The judge rejects `3` → the worker edits `add.py` to `print(1 + 1)` → the judge sees `2` →
`met:true` → the loop stops. That's the entire model.

## Run it on another agent

The judge is a shell script, so nothing here is Claude-specific — only the `agent:` and `model:` keys
in `agg.yaml` are. Edit them and re-run (or let `agg init --agent <a>` write a correct one):

```yaml
# Claude Code (as shipped)
agent: claude
model: claude-haiku-4-5-20251001
```
```yaml
# OpenAI Codex — delete `model:` entirely; Codex picks one that fits your account
agent: codex
```
```yaml
# GitHub Copilot CLI
agent: copilot
model: auto
```

Then `agg doctor` (checks the agent is installed and can do what this config asks) and `agg run`.
The loop, the judge and the gate behave identically on all three.

## Files
- `add.py` — the (broken) target the worker fixes
- `check.sh` — the judge (prints `{"met":...}`) — a plain script, so it works on any agent
- `goals.yaml` — one binary goal, `stop_when: prints_two`
- `agg.yaml` — minimal config (`agent:` + `model:`)
- `AGG_RESUME.md.template` — copy to `AGG_RESUME.md` (the live prompt is gitignored)

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

**2. A judge** (`VERIFY`) — `judges/tests.sh` runs the suite and prints a verdict:

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

**3. `goals.yaml`** — one cardinal goal, met when all 3 tests pass; the `GATE` halts after 30 min:

```yaml
goals:
  - id: tests_pass
    type: cardinal
    target: 3
    description: "All calc tests pass"
    judge: { kind: script, cmd: "./judges/tests.sh", timeout: 60 }
stop_when: "tests_pass"
halt_when: "wall_hours >= 0.5"
```

**4. `agg.yaml`** — outer-loop config + the resume prompt `INJECT`ed into each session:

```yaml
agent: claude                    # ⚠️ this file is CLAUDE-SHAPED — see "Run it on another agent"
project: calc
model: "claude-opus-4-8[1m]"
resume_prompt: "AGG_RESUME.md"
budget: { total: 2000000 }       # token ceiling  → over_budget  (a GATE guard). Works on ANY agent.
cost:   { total: 5.0 }           # $ ceiling → over_cost. CLAUDE ONLY — agg REFUSES this config on
                                 #   codex/copilot, which cannot report dollars. (API-equivalent
                                 #   price Claude reports; a usage proxy, NOT a subscription
                                 #   charge — see note below)
summary: { enabled: true, model: haiku, min_interval_secs: 1 }
memory: { enabled: true, max_kb: 64, inject_kb: 8 }   # durable AGG_MEMORY.md, on by default
```

> **Running this on Codex or Copilot?** Three Claude-shaped keys have to go:
> drop `cost:` (only Claude reports dollars — `budget:` above already caps the run and works
> everywhere), drop the `model:` **inside `summary:`** (`haiku` is a Claude name), and for the
> worker `model:` — omit it entirely on Codex (naming one is a hard 400), or use `auto` on Copilot.
> `agg doctor` catches the first and third; the summary model it cannot, so do not miss it.
> Easier: `agg init --agent codex` scaffolds a correct one for you.

> **A note on `cost` / `over_cost`.** The dollar figure is `total_cost_usd` as reported by the
> `claude` CLI — the **API-equivalent list price** of the work. On a **Max/Pro subscription you are
> not billed per token**, so this is a **usage proxy, not money charged to you**; the dashboard and
> `agg status` label it `(API-eq)` for that reason. It's still a useful ceiling (`over_cost` halts a
> runaway loop by relative spend), but read it as "how much work" not "how much money" unless you're
> actually on pay-as-you-go API billing. Prefer `over_budget` (tokens) or `over_iterations` if you
> want a plan-agnostic cap.

**5. `AGG_RESUME.md`** — the prompt `INJECT`ed into *every* fresh session:

```
GOAL: make all tests in test_calc.py pass.
calc.py has add(a,b), factorial(n), is_prime(n) stubbed with NotImplementedError.

THIS SESSION:
1. Run `python3 -m pytest -q` to see what's failing.
2. Implement the failing function(s) in calc.py — real, correct implementations.
3. Re-run pytest to confirm. You are autonomous; do the work and exit.
```

**6. Run it:**

```bash
agg plan        # dry run: one VERIFY pass, no RUN — shows "tests_pass cardinal 0/3 — loop would continue"
agg run         # the outer loop: RUN real agents until VERIFY passes, then GATE stops
agg dashboard   # (optional, second terminal) live colored TUI
```

What happens: a fresh agent reads the injected prompt, runs pytest, implements the functions,
re-runs pytest → green, exits. `VERIFY` flips the goal `0/3 → 3/3`; `GATE` sees the stop condition
met → the loop exits after one session. A one-line summary records what it did.

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

It **translates** whatever plan exists into goals + judges (it doesn't replicate your spec tooling),
asks only about genuine gaps, and writes an `agg.yaml` shaped for **your** agent — no cost guard on
Codex or Copilot, which cannot report one. Then exit the agent and `agg run`.

