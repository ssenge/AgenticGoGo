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

### Unreleased — Tier B #2 dollar budget + #10 `--json`
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
| 3 | P2 | M | **Self-updating institutional memory** | A built-in convention/file the loop manages so a worker persists "gotchas" across fresh sessions (vs. leaving it entirely to `AGG_RESUME.md` discipline). *(snarktank ships `AGENTS.md`/`progress.txt`.)* |
| 5 | P3 | M | **In-iteration context summarization** | *Deferred, not rejected.* Auto-summarize a long session as context fills, feeding the digest into the worker (vs. our current human-facing dashboard summaries). Less critical given fresh-context-per-session, but wanted eventually. *(vercel-labs' `RalphContextManager`.)* |

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
- Item 5 is **deferred** (wanted, just not now), not decided against — kept in Tier B at P3.
- Every shipped item lands with tests; the full suite + clippy + a Windows cross-check gate each release.
