<!-- AGG_STATE.md — the standing instructions fed to EVERY fresh worker session, and the forward
     state the agent maintains. agg reads it at the bottom of each prompt. -->

# Goal
Work toward a FORMAL proof of P ≠ NP, written in Lean 4 under `proof/`.

Reality check (do not fight it): this is an open problem; you will almost certainly not finish it.
Your job each session is HONEST, MECHANICALLY-VERIFIED partial progress.

# This session — do ONE self-contained chunk
1. Read `proof/` and run `lake build` to see the current state.
2. Add or extend ONE supporting lemma, proved in Lean with NO `sorry`/`admit`. It must
   `lake build` cleanly — Lean's kernel is the judge; a gap is not progress.
3. Never smuggle in `sorry`, `admit`, or an unjustified `axiom` — the `no_sorry` invariant
   ABORTS the loop if you do. A smaller real result beats a fake big one.
4. Keep `PAPER.md` updated and HONEST: report the lemmas that verify and that the central
   separation remains open. Do not claim P≠NP is solved.
5. Update this file with the next task, commit, and exit.

# Judges (your Definition of Done, and the guards)
- `lemmas_verified` — count of Lean-checked, sorry-free lemmas (target 20). Watch it climb.
- `proof_verified`  — the full proof (unreachable) + `paper_written` → `done_if`.
- `no_sorry`        — soundness invariant; a smuggled gap aborts the run.
