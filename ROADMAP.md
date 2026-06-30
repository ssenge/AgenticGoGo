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
| 5 | P3 | M | **In-iteration context summarization** | *LOW VALUE — deferred, not required today.* vercel's `RalphContextManager` summarizes a session's context mid-run as it fills (~70%) and feeds the digest BACK into the same long-running session. agg's existing summaries are between-session + outward-only (to the dashboard), never fed back to the worker — and that's by design: fresh-context-per-session means when context would fill, the session just ENDS and a fresh one starts. #5 mostly solves a problem our architecture designs away; only worth revisiting if we ever want individual sessions to run much longer before resetting. *(vercel-labs' `RalphContextManager`.)* |

> **#3 Institutional memory shipped** — see the **Unreleased** entry above for what landed
> (enforced 4-layer write, durable `AGG_MEMORY.md`, the `inject_kb` / `max_kb` caps, dashboard
> indicator).

### Tier C — large (need their own scoping conversation)
| # | Pri | Effort | Item | Notes |
|---|-----|--------|------|-------|
| 1 | P1 | L | **Parallel workers** | N concurrent workers with git-worktree isolation + merge reconciliation. Per-session git isolation is the primitive; this is the leap to "run N and reconcile." The one axis where `agg` isn't playing vs Gas Town. A cluster, not a single task. |
| 8 | P2 | L | **Richer judges** | Judge result caching; judge parallelism (judges run sequentially today); a larger bundled judge/rubric library; weighted/composite goals. A cluster. |
| 4 | P2 | L | **Web dashboard** | Remote-accessible vs. the local TUI. Lower priority — `/agg:supervise` (mobile) partly covers the need. |

---

## Notes
- Tiers A→C are roughly ordered by value/effort.
- Items 1 and 8 are **clusters** — each needs its own breakdown before implementation.
- Item 5 is **low value + deferred** — it mostly solves a problem our fresh-context-per-session
  architecture designs away (see its row). Not required today; kept in Tier B at P3.
- Item 3 **shipped** (see Unreleased): enforced read-injection (`AGG_MEMORY.md` + always-on
  `=== LAST SESSION ===` block) plus a 4-layer write-degradation floor, both token-capped.
- Every shipped item lands with tests; the full suite + clippy + a Windows cross-check gate each release.
