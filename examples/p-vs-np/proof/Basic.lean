/- Starter Lean file for the P vs NP showcase.

   The worker builds up VERIFIED supporting lemmas here (and in new files under proof/).
   Rules the judges enforce (see judges/ and the example README):
     • `lake build` must succeed (the project + Mathlib must compile).
     • NO gap-placeholder tactics anywhere — a gap is not a proof; the loop halts on it.
     • count_lemmas counts `theorem`/`lemma` declarations in gap-free, building files.

   This file starts essentially empty on purpose. The full P≠NP statement is NOT here and
   will not be completed — the point is honest, mechanically-checked partial progress. -/

namespace PvsNP

-- A trivially-true seed lemma so the project builds from cycle one (gap-free).
theorem true_intro : True := trivial

-- The worker adds real, Lean-verified supporting lemmas below.

end PvsNP
