// Small pure formatters + the judge-state → status mapping. Status colors follow the validated
// dataviz status palette and are ALWAYS paired with a glyph + label (never color alone).

/** Map a judge to { color-var, glyph, label }. A *broken* judge (error set) gets its own glyph,
 *  distinct from a clean "not met": a reader must never confuse "could not grade" with "not met". */
export function judgeStatus(j) {
  if (j?.error) return { css: 'critical', glyph: '⊘', label: 'errored' };
  switch (j?.state) {
    case 'met':         return { css: 'good',     glyph: '✔', label: 'met' };
    case 'regressed':   return { css: 'critical', glyph: '⚠', label: 'regressed' };
    case 'in_progress': return { css: 'warning',  glyph: '◐', label: 'in progress' };
    case 'pending':
    default:            return { css: 'muted',    glyph: '○', label: 'pending' };
  }
}

/** The measure a judge shows, as a descriptor the component renders (§7.4):
 *   - errored  → { kind:'error' }                         (never a lying number)
 *   - binary   → { kind:'binary', text:'met'|'unmet' }    (value is null: NOT "0")
 *   - numeric  → { kind:'numeric', text:'v / target', frac } (value/target + a bar)
 *  A binary or broken judge carries NO number (value === null), so it is met/unmet, never 0. */
export function judgeMeasure(j) {
  if (j?.error) return { kind: 'error', text: 'error' };
  if (j?.value == null) return { kind: 'binary', text: j?.met ? 'met' : 'unmet' };
  const target = j.target ?? 0;
  const frac = target > 0 ? Math.min(1, Math.max(0, j.value / target)) : (j.met ? 1 : 0);
  return { kind: 'numeric', value: j.value, target, frac, text: `${Math.round(j.value)} / ${Math.round(target)}` };
}

/** The §7.4 per-judge scoreboard the UI renders. Prefers the native `judges` (Option value/max,
 *  explicit `met`, broken-judge `error`, run-set judges like `stalled`); falls back to mapping the
 *  legacy `goals` so a state.json written by a pre-§7.4 `agg` still renders — mirrors the Rust
 *  `DashboardState::judge_views()` / `JudgeView::from_goal` bridge. */
export function judgeViews(state) {
  const j = state?.judges;
  if (Array.isArray(j) && j.length) return j;
  return (state?.goals ?? []).map(fromGoal);
}

/** Bridge a legacy GoalView into a judge view — recovers the lost null-ness from the goal type:
 *  a `binary` goal carried no real number, so its value/max come back as null (not a fake 0). */
function fromGoal(g) {
  const numeric = g.goal_type !== 'binary';
  return {
    name: g.id,
    kind: (g.judge_kind ?? 'script').startsWith('llm') ? 'llm' : (g.judge_kind || 'script'),
    in_dod: true, // the legacy `goals` set was DoD-only.
    invariant: g.invariant,
    state: g.state,
    met: g.state === 'met',
    value: numeric ? g.value : null,
    max: numeric ? g.max : null,
    target: g.target,
    delta: g.delta,
    rationale: g.rationale,
    error: null
  };
}

/** Map the loop phase to a status color for the header dot. The four outer-loop stages are
 *  inject / run / verify / gate; only RUN has a worker burning tokens, so only it reads 'good'.
 *  `staging` (a reconsider step) reads like `backoff` — the loop stepped back to think. */
export function phaseStatus(phase) {
  switch (phase) {
    case 'run':      return 'good';
    case 'inject':
    case 'verify':
    case 'gate':     return 'accent';
    case 'backoff':
    case 'staging':  return 'serious';
    case 'done':     return 'muted';
    default:         return 'accent';
  }
}

export function fmtTokens(n) {
  if (n == null) return '—';
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k';
  return String(n);
}

export function fmtDur(secs) {
  if (secs == null) return '—';
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (h > 0) return `${h}h${String(m).padStart(2, '0')}m`;
  if (m > 0) return `${m}m${String(s).padStart(2, '0')}s`;
  return `${s}s`;
}

export function fmtBytes(n) {
  if (!n) return '0 B';
  if (n >= 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + ' MB';
  if (n >= 1024) return (n / 1024).toFixed(1) + ' KB';
  return n + ' B';
}

/** A cost cell: a real price, or "—" for an agent that cannot report one (a subscription CLI).
 *  Never "$0.00" — that would lie about a run that simply can't be priced. */
export function fmtMoney(c) {
  return c == null ? '—' : `$${c.toFixed(2)}`;
}

/** A stable key per agent so claude/codex/copilot read apart at a glance (drives the row accent). */
export function agentKey(agent) {
  return ['claude', 'codex', 'copilot'].includes(agent) ? agent : 'other';
}
