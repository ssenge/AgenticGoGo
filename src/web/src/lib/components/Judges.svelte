<script>
  import { judgeStatus, judgeMeasure, judgeViews } from '$lib/format.js';
  let { state } = $props();

  const judges = $derived(judgeViews(state));
  // DoD-set drives the "done" count (§5.3); the run-set (e.g. `stalled`) is machinery, shown below.
  const dod = $derived(judges.filter((j) => j.in_dod));
  const run = $derived(judges.filter((j) => !j.in_dod));

  const met = $derived(state?.goals_met ?? dod.filter((j) => j.met).length);
  const total = $derived(state?.goals_total ?? dod.length);
  const pct = $derived(total > 0 ? Math.round((met / total) * 100) : 0);

  // lifecycle tally over the DoD-set so "1/3" says WHICH three — an errored judge (⊘) is counted
  // apart from a clean "not met", never as met.
  const tally = $derived({
    met: dod.filter((j) => j.met).length,
    prog: dod.filter((j) => j.state === 'in_progress').length,
    pend: dod.filter((j) => j.state === 'pending' && !j.error).length,
    regr: dod.filter((j) => j.state === 'regressed').length,
    err: dod.filter((j) => j.error).length
  });

  const signed = (d) => (d > 0 ? '+' : '') + Math.round(d);
</script>

{#snippet judgeRow(j)}
  {@const s = judgeStatus(j)}
  {@const m = judgeMeasure(j)}
  <li class:errored={!!j.error}>
    <span class="glyph" data-c={s.css} title={s.label}>{s.glyph}</span>
    <div class="idcol">
      <span class="id mono">{j.name}</span>
      {#if j.invariant}<span class="tag" title="invariant — a guard that must hold">guard</span>{/if}
    </div>
    <!-- binary judge: the measure cell is kept (blank) so the desktop grid columns stay aligned, but
         carries no word — the glyph + right-hand state label already say met/unmet; a "met" word here
         would just duplicate the state label. On mobile the blank cell is collapsed (below). -->
    <div class="measure" class:blank={m.kind === 'binary'}>
      {#if m.kind === 'numeric'}
        <span class="num mono">{m.text}</span>
        <span class="mini"><span class="mini-fill" data-c={s.css} style="width: {m.frac * 100}%"></span></span>
      {:else if m.kind === 'error'}
        <span class="word mono" data-c={s.css}>{m.text}</span>
      {/if}
    </div>
    {#if j.value != null && j.delta}
      <span class="delta" data-dir={j.delta > 0 ? 'up' : 'down'}>{j.delta > 0 ? '▲' : '▼'}{signed(j.delta)}</span>
    {:else}
      <span class="delta"></span>
    {/if}
    <span class="judge mono">judge:{j.kind}</span>
    <span class="statelabel" data-c={s.css}>{s.label}</span>
    {#if j.rationale}
      <div class="rationale" class:errored={!!j.error}>↳ {j.rationale}</div>
    {/if}
  </li>
{/snippet}

<section class="card panel">
  <div class="phead">
    <h2>Judges</h2>
    <div class="score mono">{met}<span class="sep">/</span>{total} met · {pct}%</div>
  </div>

  <!-- progress bar: DoD-set met fraction (§5.3) -->
  <div class="bar" role="progressbar" aria-valuenow={pct} aria-valuemin="0" aria-valuemax="100" aria-label="judges met">
    <div class="fill" style="width: {pct}%"></div>
  </div>

  <div class="tally mono">
    <span data-c="good">✔ {tally.met}</span>
    <span data-c="warning">◐ {tally.prog}</span>
    <span data-c="muted">○ {tally.pend}</span>
    <span data-c="critical" class:zero={tally.regr === 0}>⚠ {tally.regr}</span>
    <span data-c="critical" class:zero={tally.err === 0}>⊘ {tally.err}</span>
  </div>

  {#if judges.length === 0}
    <div class="empty">No judges reported yet.</div>
  {:else}
    <ul class="judges">
      {#each dod as j (j.name)}
        {@render judgeRow(j)}
      {/each}
    </ul>
    {#if run.length > 0}
      <div class="divider">run-set · not counted toward done</div>
      <ul class="judges run">
        {#each run as j (j.name)}
          {@render judgeRow(j)}
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .panel { padding: 18px 20px; }
  .phead { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 12px; }
  h2 { font-size: 14px; color: var(--ink-2); text-transform: uppercase; letter-spacing: 0.6px; }
  /* The met fraction is the Definition-of-Done headline — the single most important number on the
     page — so it reads at hero scale, not as a 14px afterthought next to the cost tile. */
  .score { font-size: 22px; font-weight: 700; color: var(--ink); }
  .sep { color: var(--ink-3); margin: 0 2px; }

  .bar {
    height: 14px;
    background: var(--progress-track);
    border-radius: 7px;
    overflow: hidden;
    margin-bottom: 12px;
  }
  .fill { height: 100%; background: var(--progress); border-radius: 7px; transition: width 0.5s ease; }

  .tally { display: flex; gap: 14px; font-size: 13px; margin-bottom: 4px; }
  .tally span[data-c='good'] { color: var(--good); }
  .tally span[data-c='warning'] { color: var(--warning); }
  .tally span[data-c='muted'] { color: var(--ink-3); }
  .tally span[data-c='critical'] { color: var(--critical); }
  .tally span.zero { color: var(--ink-3); opacity: 0.55; }

  .empty { color: var(--ink-3); font-size: 14px; padding: 8px 0; }

  .judges { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; }
  li {
    display: grid;
    grid-template-columns: 22px minmax(130px, 1.3fr) minmax(120px, 1fr) 58px minmax(84px, auto) auto;
    align-items: center;
    gap: 10px;
    padding: 10px 0;
    border-top: 1px solid var(--border);
    font-size: 14px;
  }
  li:first-child { border-top: none; }

  .glyph { font-size: 15px; text-align: center; }
  .glyph[data-c='good'] { color: var(--good); }
  .glyph[data-c='warning'] { color: var(--warning); }
  .glyph[data-c='critical'] { color: var(--critical); }
  .glyph[data-c='muted'] { color: var(--muted); }

  .idcol { display: flex; align-items: center; gap: 7px; min-width: 0; }
  .id { color: var(--ink); font-weight: 500; overflow: hidden; text-overflow: ellipsis; }
  .tag {
    font-size: 11px; color: var(--ink-3);
    border: 1px solid var(--border-strong); border-radius: 4px; padding: 0 5px; white-space: nowrap;
  }

  .measure { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .num { color: var(--ink); white-space: nowrap; }
  .word { text-transform: none; }
  .word[data-c='good'] { color: var(--good); }
  .word[data-c='warning'] { color: var(--warning); }
  .word[data-c='critical'] { color: var(--critical); }
  .word[data-c='muted'] { color: var(--ink-3); }
  .mini {
    flex: 1; min-width: 34px; height: 6px;
    background: var(--progress-track); border-radius: 3px; overflow: hidden;
  }
  .mini-fill { display: block; height: 100%; border-radius: 3px; transition: width 0.5s ease; }
  .mini-fill[data-c='good'] { background: var(--good); }
  .mini-fill[data-c='warning'] { background: var(--warning); }
  .mini-fill[data-c='critical'] { background: var(--critical); }
  .mini-fill[data-c='muted'] { background: var(--muted); }

  .delta { font-size: 13px; font-weight: 600; white-space: nowrap; }
  .delta[data-dir='up'] { color: var(--good); }
  .delta[data-dir='down'] { color: var(--critical); }
  .judge { color: var(--accent); font-size: 13px; white-space: nowrap; }
  .statelabel { font-size: 12px; justify-self: end; white-space: nowrap; }
  .statelabel[data-c='good'] { color: var(--good); }
  .statelabel[data-c='warning'] { color: var(--warning); }
  .statelabel[data-c='critical'] { color: var(--critical); }
  .statelabel[data-c='muted'] { color: var(--muted); }

  .rationale { grid-column: 2 / -1; color: var(--ink-3); font-size: 13px; margin-top: 2px; }
  .rationale.errored { color: var(--critical); }

  .divider {
    margin: 6px 0 2px; padding-top: 10px; border-top: 1px dashed var(--border-strong);
    font-size: 11px; color: var(--ink-3); text-transform: uppercase; letter-spacing: 0.5px;
  }
  .judges.run .id { color: var(--ink-2); }

  @media (max-width: 720px) {
    /* Two-line row so the numeric measure (value + bar) SURVIVES on phones — it's the core §7.4
       value/target signal and must not vanish on the mobile-supervision surface. The state label
       stays (top-right) to carry lifecycle; only the delta and judge:kind columns drop. */
    li {
      grid-template-columns: 22px 1fr auto;
      row-gap: 6px;
    }
    .glyph { grid-column: 1; grid-row: 1; }
    .idcol { grid-column: 2; grid-row: 1; }
    .statelabel { grid-column: 3; grid-row: 1; display: block; }
    /* measure + rationale drop to their own full-width rows via auto-flow (no pinned grid-row), so a
       binary judge with no measure doesn't leave an empty second line. */
    .measure { grid-column: 2 / -1; display: flex; }
    .measure.blank { display: none; }
    .judge, .delta { display: none; }
    .rationale { grid-column: 2 / -1; }
  }
</style>
