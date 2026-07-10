# AgenticGoGo — Roadmap

The consolidated backlog of improvements, drawn from a **competitive comparison** against the
Ralph-loop tool family (see [COMPARISON.md](COMPARISON.md)) and an ongoing **architecture and
code review**. Items are grouped by status, then by tier. This is the durable backlog — chat is not.

> Legend: **P0** must-do · **P1** high-value · **P2** nice-to-have · effort **S/M/L** · ✅ done · ⬜ open · 🚫 decided-against

---

## ✅ Shipped

### v0.0.7 — clean modular architecture + config folder
Architecture-review P0/P1 cleanup and the optional config folder:
- `proc.rs` / `util.rs` / `paths.rs` extraction — eliminated OS-primitive duplication (kill-FFI 6→1, etc.)
- `[lib]` target + end-to-end integration harness (real binary vs a fake `claude`)
- foreground double-run guard; poisoned-mutex recovery (never crash the loop)
- `git::resolve_session` extracted + merge/veto truth table unit-tested
- dead-code removal; stale dev-narrative comment sweep
- optional `agg/` config folder (`agg init --folder`; auto-detected at run time)
- Windows `localtime_r` link fix (the binary never actually built on Windows before)

### v0.0.8 — Tier 1 polish + Tier 2 robustness
- `agg status` reads the cheap `.agg/state.json` snapshot instead of re-running judges
- `agg dashboard --once` — one-shot snapshot to stdout (headless/CI/SSH)
- top-level `agg budget <n>` alias (steering-verb symmetry)
- friendly resume-prompt error pointing at a sibling `.template` (example footgun); logo → `assets/`
- Windows scoped honestly (unix-first; degraded watchdog/spawn-protection disclosed at runtime + README)
- watchdog pid-reuse race closed with a `reaping` handoff gate

### v0.0.9 — Tier A small items
- **`agg history`** — run-history ledger surfaced (newest-first runs + lifetime totals); also fixed a
  latent gap where already-satisfied/halted-at-baseline runs recorded nothing
- **Doctor checks script-judge files** exist + are executable (with an exact `chmod +x` hint)
- **`agg judge <id>`** — run one judge, print its raw verdict (raw JSON to stdout, human line to stderr)

### v0.0.10 — dollar budget + institutional memory + `--json` + rollback gate
*(#2 dollar budget + `over_iterations`, #3 institutional memory, #10 `--json`, #11 Phase 1 rollback gate)*
- **#3 Institutional memory** — built-in, agg-managed cross-session learning, on by default with
  zero setup. A durable, committable **`AGG_MEMORY.md`** at the project root holds rolled-up
  learnings. **Enforced, never trusting the worker:** agg writes memory itself after *every*
  session (even a crashed/killed/ignoring worker) via a 4-layer design — (3a) fold an optional
  worker-written `.agg/memory/session-<N>.md` note on a clean session; (3b) else the freshly
  computed windowed summary; (3c) else the mechanical facts (exit/scoreboard/goal-deltas) as the
  always-produces-content floor — plus an always-on `=== LAST SESSION ===` read-back block
  (prior cycle's goal deltas + scoreboard) prepended to every worker prompt. Two independent
  caps keep it token-safe: **`inject_kb`** (default 8 KB) bounds per-prompt injection
  independently of **`max_kb`** (default 64 KB on disk, oldest entries drop first); env overrides
  `AGG_MEMORY_INJECT_KB` / `AGG_MEMORY_MAX_KB` (0 = uncapped). Configured via
  `memory: { enabled, max_kb, inject_kb }`; dashboard/status show a `memory <size>` indicator
  once the file has content.
- **#2 Dollar-denominated budget** (`cost.total: $N` → `over_cost`). **No pricing table** — the
  original plan assumed one, but `claude -p` already emits `total_cost_usd` on each session's
  result event (correct per-model, `[1m]`-variant- and cache-aware), so agg just sums that float.
  Mirrors the token plumbing exactly (`cost_spent`/`cost_limit`/`over_cost` alongside
  `tokens_spent`/`budget_total`/`over_budget`). Also exposed **`over_iterations`** (sessions cap,
  backed by `--max-sessions`) so all three ceilings — tokens, dollars, sessions — read uniformly
  as `over_*` and OR together in `halt_when`.
- **#10 Structured output (`--json`)** — `agg status --json` (full `DashboardState` snapshot) and
  `agg history --json` (full `Project` ledger), reusing the existing serde types.
- **#11 Post-merge rollback gate** (Phase 1 of the salvaged #1 hardening). Fixes a LIVE bug: the
  loop merges + commits a session before judging, so a red post-merge judge used to leave the bad
  merge on base with no rollback. Now (when `session_isolation.rollback_on_regression` is on —
  default) agg STAGES the merge (`merge --no-ff --no-commit`), re-runs the judges against the
  merged tree, then COMMITS it (keep) or ABORTS it (roll back — base stays pristine, branch kept
  for inspection) based on whether a previously-met goal **regressed**. A judge that merely
  *couldn't run* (timeout/spawn-fail/rate-limit/bad-JSON) never triggers rollback — only a real
  met→not-met regression does. Also fixed: judge `"diff"` inputs now resolve to `HEAD^..HEAD` when
  the tree is clean (post-commit), so a `diff`-input goal isn't silently empty. New git primitives
  `stage_session`/`finalize_session`; rate-limit check moved before merge. *(Phase 0 — worktree
  isolation — still open in Tier B #11.)*

### v0.0.11 — security & correctness hardening
*(Harden the judges against worker-controlled input, fix the rollback-gate defects, and land the
packaging table-stakes.)*
- **Judge moat hardened.** The LLM judge no longer loads the worker-mutated repo's config: it runs
  with `--setting-sources user` (only the operator's own settings/hooks, never the repo's
  `.claude/settings.json`), and every untrusted judge input (file contents / `git diff`) is wrapped
  in a per-invocation **nonce fence** with a de-instruction preamble, and any forged fence marker in
  the content is neutralized — so a worker can't inject a verdict by writing to a judged file. Same
  isolation applied to the summarizer. *(Residual: CLAUDE.md auto-discovery in the judge's cwd still
  needs worktree isolation — Tier B #11 Phase 0.)*
- **P≠NP demo judges made sound.** `verify_proof.sh`/`no_sorry.sh` now reject smuggled `axiom`
  declarations and unsound escape hatches (`sorryAx`, `native_decide`/`ofReduceBool`), not just
  `sorry`; `count_lemmas.sh` no longer counts trivial `: True` padding. The false "uncheatable"
  comments are corrected.
- **Rollback-gate defects fixed.** (1) A no-commit session resolves as a new `NoChanges` outcome
  instead of entering the commit/abort path whose fallback ran `reset --hard` and destroyed a
  worker's uncommitted work. (2) The sticky-`Regressed` clause is deleted — the gate now keys only
  on a judge-ran per-cycle delta, so one regression no longer vetoes every future merge. (3) On a
  rollback the engine state is snapshot-restored to base truth and stop/halt is recomputed, so the
  loop never reports success on discarded work or poisons memory with phantom deltas; the memory
  entry is stamped "session ROLLED BACK". (4) Judge `"diff"` inputs use `git diff HEAD`, so a staged
  merge is scored — not the previous session's diff.
- **Meaningful exit codes.** `agg run` returns 0 (goals-met / operator-stop), 3 (halt), 4
  (max-sessions), 1 (error) so automation can branch on the outcome; the paused-stop path no longer
  `process::exit`s past the Drop guards, and the max-sessions exit now publishes its finished state.
- **`worker_args`** — pass extra `claude` flags (e.g. `--allowedTools`) to constrain the otherwise
  unrestricted worker; the permission bypass is now documented in the README.
- **Table-stakes.** MIT `LICENSE` file added; the false Homebrew install claims and the stale
  `HANDOFF.md` are removed; the `grep_count.sh` plugin judge is hardened against option-injection
  and invalid-JSON-on-quote. *(The verification gate — clippy `-D warnings` + full test suite +
  Windows cross-check — stays a local pre-release step; no per-push CI, by choice.)*

---

## ⬜ Open

### Tier B — medium
| # | Pri | Effort | Item | Notes |
|---|-----|--------|------|-------|
| 11 | P2 | M | **Worktree isolation** (Phase 0 of the #1 hardening; Phase 1 rollback gate SHIPPED — see Unreleased) | git-**worktree** isolation so a running session never mutates the operator's working tree / `.git/HEAD` (today it's branch-checkout in one tree). New `worktree.rs`; rewrite `resolve_session`/`stage_session`'s checkout-into-base into merge-into-base-worktree; split the worker's single-`dir` contract into base-dir vs worktree-dir. A robustness win (operator can inspect base mid-run; cleaner crash recovery), and the prerequisite if N>1 were ever reconsidered. Lower priority now that the live rollback bug (Phase 1) is fixed. See the scoping under 🚫 #1. |

> **#3 Institutional memory shipped** — see the **Unreleased** entry above for what landed
> (enforced 4-layer write, durable `AGG_MEMORY.md`, the `inject_kb` / `max_kb` caps, dashboard
> indicator).

### Tier C — large (need their own scoping conversation)
| # | Pri | Effort | Item | Notes |
|---|-----|--------|------|-------|
| 8 | **P1** | L | **Richer judges** | **Now the top large item** (the #1 scoping found it strictly dominates parallel workers). Judge **parallelism** (judges run sequentially today — they're read-only + independent, so embarrassingly parallel; this speeds EVERY iteration of the loop agg *is*) + result caching + a larger bundled judge/rubric library + weighted/composite goals. Also the prerequisite that could ever make a re-test merge gate cheap enough to revisit worker concurrency. A cluster. |
| 4 | P2 | L | **Web dashboard** | Remote-accessible vs. the local TUI. Lower priority — `/agg:supervise` (mobile) partly covers the need. Identity-neutral, low risk. |
| — | — | — | *(#1 parallel workers — N>1 concurrency rejected; see 🚫 below. Its salvageable N=1 value is Tier B #11.)* | |

---

## 🚫 Decided against

### #5 — In-iteration context summarization
vercel's `RalphContextManager` summarizes a session's context mid-run as it fills (~70%) and
feeds the digest BACK into the *same* long-running session. **We will not build this.**

The reasoning turns on a distinction between two ways a task can be "too big for one session":

- **Case A — the task decomposes; you just need continuity across the splits.** A big refactor, a
  large test suite, a multi-file migration. The *common* case, and `agg` already serves it: each
  fresh session does a chunk → commits → judges gate it → the next fresh session continues from
  the filesystem + `AGG_MEMORY.md` (#3). The task fitting in one session was never the
  requirement — *progress being durable between sessions* is. #5 adds nothing here.
- **Case B — the task is genuinely atomic: one indivisible chain of reasoning that overflows a
  single context window and can't be checkpointed.** This is the only case #5 addresses — and it
  is **rare enough not to design for**. Almost everything that looks atomic is Case A with a
  missing seam (understand-a-codebase → per-subsystem passes; prove-a-theorem → lemmas; see the
  `p-vs-np` example). For the genuine remainder, the in-architecture escape hatch already exists:
  `resume_sessions: true` (opt-in context carry-over for a tightly-scoped run) — not an automatic
  mid-run context manager.

Building #5 would mean *reintroducing* long, degrading sessions and then adding machinery to fight
that degradation — taking on the exact failure mode fresh-context-per-session exists to avoid, to
serve a case that's too rare to justify it. If Case B ever bites in practice, the move is to harden
`resume_sessions` (e.g. a token ceiling that forces a fresh start), **not** the vercel-style
mid-session summarizer. *(vercel-labs' `RalphContextManager`.)*

### #1 — Parallel workers (N>1 concurrency)
N concurrent `claude -p` workers, each in a git worktree, with merge reconciliation. After a full
scoping pass (3 code scouts + prior-art research + 3 adversarial challenges), **N>1 concurrency is
rejected — not deferred.** The salvageable N=1 value (worktree isolation + a post-merge rollback
gate) is unbundled as **Tier B #11**; only the concurrency is killed.

Three reasons:

1. **The throughput paradox is the refutation, not a risk to manage.** Correctness forces a
   serialize-on-red landing gate (re-test the merged base before it advances), so base moves one
   re-test at a time. On a single machine with shell-out judges (`cargo test`, `claude -p`), that
   gate dominates wall-clock; the only win is overlapped *thinking* time — a small constant at
   N=2–3, plausibly **<1×** once merge-conflict re-queues burn fresh workers' full token budgets
   redoing discarded work. The safe config (small N, serial gate, disjoint shards) is precisely the
   config that barely speeds anything up.
2. **Decomposition makes it self-defeating.** The only thesis-safe way to give N workers
   non-colliding work (no LLM planner in the loop) is operator-declared disjoint-file shards — which
   is exactly what an operator can already do in two terminal tabs with zero new code. The moment
   shards touch a shared file (`Cargo.toml`, `lib.rs`, a shared types module — normal Rust), merge
   thrash and shared-mutable-state races bite. It works only where it's unnecessary, breaks where it
   would help.
3. **It erodes `agg`'s identity and regresses shipped features.** `agg` is the hardened
   single-machine *sequential* loop (Gas Town owns the parallel fleet). N writers dissolve the
   single-writer simplicity that makes the merge truth-table trustworthy; the cost ceiling (#2,
   post-hoc by design) becomes "ceiling + up to N−1 sessions of overshoot." And the repo's own most
   relevant prior art — worker-level `ultracode` fan-out — was tried and reverted (2026-06-10:
   parked waiting for a re-invoke that never comes).

Upstream alternative: **#8 (judge parallelism + caching)** strictly dominates #1 — it speeds every
sequential iteration AND is the precondition that could ever make a re-test gate cheap enough to
reconsider concurrency. If N>1 is ever revisited, the bar is: only after #8 ships, only after #11
ships standalone, and only on a real goal where disjoint shards demonstrably beat two terminal tabs.

---

## Notes
- Tiers A→C are roughly ordered by value/effort.
- Item 8 is a **cluster** — needs its own breakdown before implementation. It is now the top large
  item (the #1 scoping found judge parallelism strictly dominates parallel workers).
- Item 1 (parallel workers, N>1) is **decided against** after a full scoping pass (see 🚫); its
  salvageable N=1 value is unbundled as Tier B #11.
- Item 5 is **decided against** (see the 🚫 section) — it serves only the rare "genuinely atomic
  task" case (Case B); the common "big task that decomposes" case (Case A) is already served by
  fresh sessions + `AGG_MEMORY.md`, and the rare remainder by opt-in `resume_sessions`.
- Item 3 **shipped** (see Unreleased): enforced read-injection (`AGG_MEMORY.md` + always-on
  `=== LAST SESSION ===` block) plus a 4-layer write-degradation floor, both token-capped.
- Every shipped item lands with tests; the full suite + clippy + a Windows cross-check gate each release.
