<script>
  import { goalStatus, goalMeasure } from '$lib/format.js';
  let { state } = $props();

  const goals = $derived(state?.goals ?? []);
  const met = $derived(state?.goals_met ?? 0);
  const total = $derived(state?.goals_total ?? 0);
  const pct = $derived(total > 0 ? Math.round((met / total) * 100) : 0);
</script>

<section class="card panel">
  <div class="phead">
    <h2>Goals</h2>
    <div class="score mono">{met}<span class="sep">/</span>{total} met · {pct}%</div>
  </div>

  <!-- progress bar: single-headline magnitude, 4px rounded data-end anchored to the track baseline -->
  <div class="bar" role="progressbar" aria-valuenow={pct} aria-valuemin="0" aria-valuemax="100" aria-label="goals met">
    <div class="fill" style="width: {pct}%"></div>
  </div>

  {#if goals.length === 0}
    <div class="empty">No goals reported yet.</div>
  {:else}
    <ul class="goals">
      {#each goals as g (g.id)}
        {@const s = goalStatus(g.state)}
        <li>
          <span class="glyph" data-c={s.css} title={s.label}>{s.glyph}</span>
          <div class="idcol">
            <span class="id mono">{g.id}</span>
            {#if g.invariant}<span class="tag">guard</span>{/if}
            {#if g.latched}<span class="tag latch" title="latched — no longer re-judged">🔒</span>{/if}
          </div>
          <span class="type mono">{g.goal_type}</span>
          <span class="measure mono">{goalMeasure(g)}</span>
          {#if g.delta > 0}<span class="delta">▲+{Math.round(g.delta)}</span>{/if}
          <span class="judge mono">judge:{g.judge_kind}</span>
          <span class="statelabel" data-c={s.css}>{s.label}</span>
          {#if g.rationale}
            <div class="rationale">↳ {g.rationale}</div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .panel { padding: 18px 20px; }
  .phead { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 12px; }
  h2 { font-size: 14px; color: var(--ink-2); text-transform: uppercase; letter-spacing: 0.6px; }
  .score { font-size: 14px; color: var(--ink); }
  .sep { color: var(--ink-3); margin: 0 2px; }

  .bar {
    height: 10px;
    background: var(--progress-track);
    border-radius: 5px;
    overflow: hidden;
    margin-bottom: 16px;
  }
  .fill {
    height: 100%;
    background: var(--progress);
    border-radius: 5px;
    transition: width 0.5s ease;
  }

  .empty { color: var(--ink-3); font-size: 14px; padding: 8px 0; }

  .goals { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; }
  li {
    display: grid;
    grid-template-columns: 22px minmax(140px, 1.4fr) 90px 110px 54px minmax(120px, 1fr) auto;
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
    border: 1px solid var(--border-strong); border-radius: 4px; padding: 0 5px;
  }
  .tag.latch { border: none; padding: 0; }
  .type { color: var(--ink-3); font-size: 13px; }
  .measure { color: var(--ink); }
  .delta { color: var(--good); font-size: 13px; font-weight: 600; }
  .judge { color: var(--accent); font-size: 13px; }
  .statelabel { font-size: 12px; justify-self: end; }
  .statelabel[data-c='good'] { color: var(--good); }
  .statelabel[data-c='warning'] { color: var(--warning); }
  .statelabel[data-c='critical'] { color: var(--critical); }
  .statelabel[data-c='muted'] { color: var(--muted); }

  .rationale {
    grid-column: 2 / -1;
    color: var(--ink-3);
    font-size: 13px;
    margin-top: 2px;
  }

  @media (max-width: 720px) {
    li { grid-template-columns: 22px 1fr auto; }
    .type, .judge, .statelabel, .delta { display: none; }
  }
</style>
