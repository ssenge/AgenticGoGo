// Small pure formatters + the goal-state → status mapping. Status colors follow the validated
// dataviz status palette and are ALWAYS paired with a glyph + label (never color alone).

/** Map a goal lifecycle state to { color-var, glyph, label }. */
export function goalStatus(state) {
  switch (state) {
    case 'met':        return { css: 'good',     glyph: '✔', label: 'met' };        // ✔
    case 'in_progress':return { css: 'warning',  glyph: '◐', label: 'in progress' };// ◐
    case 'regressed':  return { css: 'critical', glyph: '✖', label: 'regressed' };  // ✖
    case 'pending':
    default:           return { css: 'muted',    glyph: '○', label: 'pending' };    // ○
  }
}

/** Map the loop phase to a status color for the header dot. The four outer-loop stages are
 *  inject / run / verify / gate; only RUN has a worker burning tokens, so only it reads 'good'. */
export function phaseStatus(phase) {
  switch (phase) {
    case 'run':      return 'good';
    case 'inject':
    case 'verify':
    case 'gate':     return 'accent';
    case 'backoff':  return 'serious';
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

/** Goal measure string per type, mirroring the TUI scoreboard. */
export function goalMeasure(g) {
  const v = g.value ?? 0;
  switch (g.goal_type) {
    case 'binary':     return g.state === 'met' ? 'yes' : 'no';
    case 'percentage': return `${Math.round(v)} / ${Math.round(g.target ?? 0)}%`;
    case 'cardinal':   return `${Math.round(v)} / ${Math.round(g.max ?? g.target ?? 0)}`;
    default:           return String(v);
  }
}
