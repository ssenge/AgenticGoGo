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

---

## ⬜ Open

### Tier A — small, high-value (next up)
These are cheap and self-contained — the immediate work.

| # | Pri | Effort | Item | Notes |
|---|-----|--------|------|-------|
| 6 | P1 | S | **`agg history`** — surface the run-history ledger | `.agg/project.json` already records lifetime sessions/tokens per run; the `Project::lifetime_sessions`/`lifetime_tokens` accessors exist but are unsurfaced. Add a command that prints prior runs + a simple trend. |
| 7 | P1 | S | **Doctor: check judge *script* files exist / are executable** | `agg doctor` already checks rubric files; a broken script `cmd:` path is the most common post-parse failure and currently goes undiagnosed. (Skip inline-shell cmds; only path-like cmds.) |
| 9 | P1 | S | **`agg judge <id>` — run one judge, print its raw verdict** | For authoring/debugging a judge without a full `plan` (which runs them all). Prints the verdict JSON + a human line. |

### Tier B — medium
| # | Pri | Effort | Item | Notes |
|---|-----|--------|------|-------|
| 2 | P1 | M | **Dollar-denominated budget** (`cost.total: $N`) | `agg`'s budget is output-tokens today. Add a `$` ceiling with a per-model pricing table, atop the existing `tokens_spent` plumbing + the `over_budget` stop term. *(vercel-labs has this; we don't.)* |
| 3 | P2 | M | **Self-updating institutional memory** | A built-in convention/file the loop manages so a worker persists "gotchas" across fresh sessions (vs. leaving it entirely to `AGG_RESUME.md` discipline). *(snarktank ships `AGENTS.md`/`progress.txt`.)* |
| 10 | P2 | M | **Structured run output (`--json`)** | Machine-readable status/result for scripting + a future web dashboard. |

### Tier C — large (need their own scoping conversation)
| # | Pri | Effort | Item | Notes |
|---|-----|--------|------|-------|
| 1 | P1 | L | **Parallel workers** | N concurrent workers with git-worktree isolation + merge reconciliation. Per-session git isolation is the primitive; this is the leap to "run N and reconcile." The one axis where `agg` isn't playing vs Gas Town. A cluster, not a single task. |
| 8 | P2 | L | **Richer judges** | Judge result caching; judge parallelism (judges run sequentially today); a larger bundled judge/rubric library; weighted/composite goals. A cluster. |
| 4 | P2 | L | **Web dashboard** | Remote-accessible vs. the local TUI. Lower priority — `/agg:supervise` (mobile) partly covers the need. |

### 🚫 Decided against (kept so we don't re-litigate)
| # | Item | Why not |
|---|------|---------|
| 5 | **In-iteration context summarization** | Fresh-context-per-session (the core Ralph discipline) makes it unnecessary by design — it's a patch for the single-session-degradation problem `agg` doesn't have. Our `summary.rs` is human-facing digests, a different category. |

---

## Notes
- Tiers A→C are roughly ordered by value/effort. Tier A is the immediate work.
- Items 1 and 8 are **clusters** — each needs its own breakdown before implementation.
- Every shipped item lands with tests; the full suite + clippy + a Windows cross-check gate each release.
