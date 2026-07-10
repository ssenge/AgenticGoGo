# p-vs-np — full-feature showcase

⚠️ **This loop will NOT solve P ≠ NP. That's the point.** It showcases the whole agg feature
set on a famous problem: measurable partial progress, a check that can't be faked, a soundness
guard that halts on cheating, an LLM-judged paper, hooks, and a budget.

The proof is written in **[Lean 4](https://lean-lang.org/)** — the formal proof assistant
mathematicians actually use. Lean's kernel rejects any proof with a gap, so `proof_verified`
literally cannot be faked, and `sorry` (Lean's "gap here" placeholder) trips a soundness
invariant that halts the loop. The worker won't finish P≠NP — it produces *verified supporting
lemmas* (real progress) and a paper.

## Features demonstrated
- **cardinal** goal `lemmas_verified` — partial progress you can watch climb
- **binary** goal `proof_verified` — the (unreachable) prize
- **LLM judge** `paper_written` (haiku + rubric) with **`recheck: once_met`** (latched)
- **invariant** `no_sorry` + **`halt_when`** — stops the loop if it cheats
- **hooks** (`on_start: lake build`) — wires the Lean toolchain (agg stays tool-agnostic)
- **budget** ceiling — a hopeless run can't bankrupt you

## Run it (needs the Lean toolchain)
```bash
# 1. install Lean 4 + lake (elan): https://lean-lang.org/  then `lake init` / add Mathlib
# 2.
cp AGG_RESUME.md.template AGG_RESUME.md
chmod +x judges/*.sh
agg run        # watch lemmas_verified climb; it will not reach proof_verified
```

## Files
- `goals.yaml` — the 4 goals + stop/halt
- `agg.yaml` — opus worker, budget, `on_start` Lean hook
- `judges/verify_proof.sh` — Lean compiles + no sorry → the real check
- `judges/count_lemmas.sh` — counts Lean-verified, sorry-free lemmas (partial progress)
- `judges/no_sorry.sh` — soundness invariant (halts on sorry/admit)
- `rubrics/paper.md` — LLM rubric (honesty is the pass criterion)
- `proof/Basic.lean` — starter Lean file (the worker grows it)
- `AGG_RESUME.md.template` — copy to `AGG_RESUME.md` (live prompt is gitignored)

---

## Showcase: "prove P ≠ NP" — every feature on one famous problem

> ⚠️ **This loop will not solve P ≠ NP. That's the point.** It's a showcase of the full machinery
> on a hard, open-ended research problem — measurable *partial* progress, a `VERIFY` gate that
> **cannot be faked**, an invariant that halts on cheating, and a paper. Point the same structure
> at *your* research problem; swap the checker for your domain.

The trick that makes this honest rather than theatre is putting a truly deterministic checker in
`VERIFY`: the proof is written in **[Lean 4](https://lean-lang.org/)**, the formal proof assistant
mathematicians use today (its `Mathlib` is the largest formal-math corpus in existence). Lean's
kernel mechanically rejects any proof with a gap, so **"verified" literally cannot be faked** — and
`sorry` (Lean's "I gave up here" placeholder) is caught by a soundness invariant that halts the
loop. The agent won't finish P≠NP (nobody has), but it produces *verified supporting lemmas* — real,
checkable progress — and a paper. *(Needs the Lean toolchain; wired in via an `on_start` hook.)*

**`goals.yaml`** — multiple goal types, an LLM judge, invariants, sticky re-checking:
```yaml
goals:
  - id: proof_verified            # the (unreachable) prize: Lean checks the full proof
    type: binary
    judge: { kind: script, cmd: "./judges/verify_proof.sh", timeout: 1800 }

  - id: lemmas_verified           # MEASURABLE PARTIAL PROGRESS: N Lean-checked lemmas
    type: cardinal
    target: 20
    judge: { kind: script, cmd: "./judges/count_lemmas.sh", timeout: 1800 }

  - id: paper_written             # qualitative → an LLM (haiku) judge with a rubric
    type: binary
    recheck: once_met             # latch it: don't re-judge the paper every cycle
    judge: { kind: llm, model: haiku, rubric: "rubrics/paper.md", inputs: ["PAPER.md"] }

  - id: no_sorry                  # SOUNDNESS GUARD: no `sorry`/`admit`/stray axiom — ever
    type: binary
    invariant: true
    judge: { kind: script, cmd: "./judges/no_sorry.sh" }

stop_when: "proof_verified AND paper_written"     # the prize (don't hold your breath)
halt_when: "not no_sorry"                         # GATE stops instantly if it smuggles in a gap
```

**`judges/verify_proof.sh`** — the real, unfakeable `VERIFY` (Lean compiles, no gaps, no smuggled axioms):
```bash
#!/usr/bin/env bash
# lake build must succeed AND the soundness check (no sorry / axiom / native_decide) must pass.
here="$(cd "$(dirname "$0")" && pwd)"
out="$(lake build 2>&1)"; rc=$?
sound="$("$here/no_sorry.sh")"
if [ $rc -eq 0 ] && printf '%s' "$sound" | grep -q '"met":true'; then
  echo '{"met":true,"rationale":"Lean built the full proof and the soundness check passed"}'
else
  echo "{\"met\":false,\"rationale\":\"Lean does not accept the proof yet (rc=$rc)\"}"
fi
```

**`agg.yaml`** — wires the Lean toolchain via a hook (`agg` stays tool-agnostic; *you* supply it):
```yaml
project: p-vs-np
model: "claude-opus-4-8[1m]"
resume_prompt: AGG_RESUME.md
budget: { total: 50000000 }              # a hard GATE ceiling — this one could run forever
hooks:
  on_start: ["lake build || true"]        # fetch/build the Lean project + Mathlib once
summary: { enabled: true, model: haiku }
```

What you get even though the prize is unreachable: a steadily-growing count of **Lean-verified
lemmas** (real progress on the dashboard), a paper, a loop that **halts the moment it tries to
cheat**, and a hard token ceiling so a hopeless run can't bankrupt you. The full feature set —
every goal type, script + LLM judges, invariants, `halt_when`, `recheck`, hooks, budget — on a
problem everyone recognizes.

