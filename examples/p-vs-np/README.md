# p-vs-np — full-feature showcase

⚠️ **This loop will NOT solve P ≠ NP. That's the point.** It showcases the whole agg feature
set on a famous problem: measurable partial progress, a check that can't be faked, a soundness
guard that aborts on cheating, an LLM-judged paper, a **vendor-diverse `reconsider` step**, hooks,
and a token budget.

The proof is written in **[Lean 4](https://lean-lang.org/)** — the formal proof assistant
mathematicians actually use. Lean's kernel rejects any proof with a gap, so `proof_verified`
literally cannot be faked, and `sorry` (Lean's "gap here" placeholder) trips a soundness
invariant that aborts the loop. The worker won't finish P≠NP — it produces *verified supporting
lemmas* (real progress) and a paper.

## Layout

There is no `goals.yaml`: **a judge IS a goal, resolved by NAME** from `agg/judges/<name>.{sh,md}`.
The name a judge is referenced by in `done_if` / `invariants` **is its filename**.

```
p-vs-np/
  proof/Basic.lean              # the Lean work the worker grows — at the project root (judges read it)
  agg/
    agg.yaml                    # defaults / judge / steps / sequence
    AGG_STATE.md                # the worker's standing instructions
    judges/
      proof_verified.sh         #   → the `proof_verified` name (was verify_proof.sh)
      lemmas_verified.sh        #   → the `lemmas_verified` name (was count_lemmas.sh)
      no_sorry.sh               #   → the `no_sorry` invariant
      paper_written.md          #   an .md judge = an LLM rubric; inputs in its own frontmatter
```

The `.md` judge is the LLM-as-judge: **the file IS the rubric**, and it declares what it reads in a
YAML frontmatter (`inputs: ["PAPER.md"]`) — one self-contained file, no registry, no `kind:` tag.
The `.sh`/`.md` extension is what decides script-vs-LLM.

## Features demonstrated
- **numeric judge** `lemmas_verified` — emits `value`/`max`, so the dashboard shows `N/20` climbing
  (and `done_if` could read the number directly with the accessor `lemmas_verified.value >= 20`)
- **binary judge** `proof_verified` — the (unreachable) prize
- **LLM judge** `paper_written` — an `.md` rubric graded on the cheap RULER model
- **invariant** `no_sorry` + **`abort_if: any_regressed(invariants)`** — aborts the run if it cheats
- **per-step agents** — a `worker x4` grunt loop on Opus, then `if stalled then reconsider` on a
  **different vendor (Codex)**: a Claude rabbit hole is not necessarily a Codex one
- **`skip_judges`** on the reconsider step — nothing merges; its work stages and the next worker gates it
- **hooks** (`on_start: lake build`) — wires the Lean toolchain (agg stays tool-agnostic)
- **budget** ceiling — a hopeless run can't run forever

## Run it (needs the Lean toolchain, and Codex for the reconsider step)
```bash
# 1. install Lean 4 + lake (elan): https://lean-lang.org/  then `lake init` / add Mathlib
# 2. authenticate Codex too (the reconsider step drives it), or change that step to `claude`
agg doctor      # checks BOTH agents the sequence names, and that budget (not cost) is the guard
agg plan        # dry run: evaluate every judge once, print the scoreboard
agg run         # watch lemmas_verified climb; it will not reach proof_verified
```

`agg plan` prints a scoreboard even before Lean is installed (the Lean judges just report "does not
build yet"):

```
Goals: 1/3   done_if: proof_verified AND paper_written
  · proof_verified     script     0/1   — Lean does not accept the proof yet …
  · paper_written      llm        0/1   — PAPER.md does not exist …
  ✔ no_sorry           script     1/1   — no gap, no smuggled axiom …
  · stalled            script     no   — no verdicts.jsonl yet — nothing to stall on
```

Note the scoreboard is **1/3** (the Definition-of-Done set: `proof_verified`, `paper_written`,
`no_sorry`) while **4 judges** ran — `stalled` is evaluated because the sequence branches on it
(`if stalled then reconsider`), but it is not part of the DoD, so it never counts toward "done".

## The two moats on show

**1. The judge is unfakeable.** `proof_verified.sh` requires `lake build` to succeed AND the
soundness check to pass. `no_sorry.sh` forbids gap placeholders (`sorry`/`admit`), hand-declared
`axiom`s, and unsound escape hatches (`native_decide`/`ofReduceBool`) — the loopholes a
"just make the judge green" worker will reach for. Because agg runs the judge and the worker never
does, none of it can be gamed from inside a session.

```bash
#!/usr/bin/env bash
# agg/judges/proof_verified.sh (abridged) — lake builds AND the soundness check passes.
here="$(cd "$(dirname "$0")" && pwd)"
out="$(lake build 2>&1)"; rc=$?
sound_met="$("$here/no_sorry.sh" | grep -o '"met":[a-z]*' | head -1 | cut -d: -f2)"
if [ $rc -eq 0 ] && [ "$sound_met" = "true" ]; then
  echo '{"met":true,"rationale":"Lean built the full proof and the soundness check passed"}'
else
  echo "{\"met\":false,\"rationale\":\"Lean does not accept the proof yet (rc=$rc)\"}"
fi
```

**2. When it stalls, a different vendor reconsiders.** Most sessions are grunt work (`worker x4` on
Opus). Only when `stalled` fires — the run has made no verdict progress across the last few merged
steps — does the sequence step back on **Codex**, told to assume the current route is a dead end and
name a different one. Its `skip_judges: true` means nothing merges from that step: the work stages,
and the next worker step gates the whole span. The rejected routes reach institutional memory
(`AGG_MEMORY.md`, agg-owned) through the worker's scratch note, so no future session repeats them.

```yaml
steps:
  worker: {}
  reconsider:
    agent: codex                 # a DIFFERENT VENDOR — perspective diversity breaks a local optimum
    prompt: "Assume the current strategy is a dead end. Name 2-3 different routes…"
    skip_judges: true            # stages; the next worker step gates it
sequence:
  steps:
    - worker x4
    - if stalled then reconsider
```

## Porting to another agent

The config names two agents already (`claude` worker, `codex` reconsider). To change either, edit
`defaults.agent` / the step's `agent:`. Two rules the capability check enforces at startup:

- **`cost:` is Claude-only.** Because this sequence names a `codex` step, a dollar guard could never
  cover the whole run — so this config uses **`budget:` (tokens)**, which works on every agent. Add
  `cost:` back only in an all-Claude sequence. `agg doctor` refuses the contradiction outright.
- **Codex: omit `model:`** (naming one is a hard 400). **Copilot: `model: auto`** with no `effort:`.

`agg doctor` checks **every** agent the sequence names, not just one.

## Files
- `agg/agg.yaml` — defaults/judge/steps/sequence: two agents, a stall-triggered reconsider, budget
- `agg/AGG_STATE.md` — the worker's standing instructions
- `agg/judges/proof_verified.sh` — Lean compiles + no smuggled assumptions → the real check
- `agg/judges/lemmas_verified.sh` — counts Lean-verified, sorry-free lemmas (partial progress)
- `agg/judges/no_sorry.sh` — the soundness invariant (aborts on sorry/admit/axiom)
- `agg/judges/paper_written.md` — the LLM rubric (honesty is the pass criterion)
- `proof/Basic.lean` — starter Lean file (the worker grows it)
