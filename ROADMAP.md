# AgenticGoGo — Roadmap

The consolidated backlog of improvements, drawn from two analyses: the **competitive comparison**
against the Ralph-loop tool family (see [COMPARISON.md](COMPARISON.md)) and a **Senior-Lead-Architect
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

### Unreleased — Tier B #2 dollar budget + #3 institutional memory + #10 `--json`
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

---

## ⬜ Open

### Tier B — medium
| # | Pri | Effort | Item | Notes |
|---|-----|--------|------|-------|
| 11 | P1 | M | **Isolation + rollback hardening** (the salvaged value of #1) | Two N=1 sequential-loop wins that fell out of the #1 scoping. **Phase 0:** git-**worktree** isolation so a running session never mutates the operator's working tree / `.git/HEAD` (today it does — branch-checkout in one tree). **Phase 1:** a **post-merge rollback gate** — fixes a LIVE bug: today `merge_no_ff` commits immediately and judges run post-merge, so a red post-merge judge leaves the bad merge on base with no rollback. Split into `merge --no-commit` → re-test → `commit | abort`; re-point judge `"diff"` inputs to `base..HEAD`; force `recheck: always` on the gate; never roll back on a *can't-run* judge (only on a real regression). See the scoping under 🚫 #1. |

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
