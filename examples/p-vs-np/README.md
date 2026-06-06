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
