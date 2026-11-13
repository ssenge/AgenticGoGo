<script>
  import { onMount, onDestroy } from 'svelte';

  // The run report of a process run (agg process): where the run is (map) and where the time went (profile).
  let map = $state(null);
  let profile = $state(null);
  let error = $state('');
  let timer;

  const CATS = ['agent', 'repair', 'critic', 'check', 'hooks', 'force', 'human', 'driver'];

  async function load() {
    try {
      const [m, p] = await Promise.all([fetch('/api/process?report=map'), fetch('/api/process?report=profile')]);
      if (m.status === 404) {
        error = 'This project runs no process (agg process <file> <project>).';
        return;
      }
      if (!m.ok || !p.ok) throw new Error('agg serve answered ' + (m.ok ? p.status : m.status));
      map = await m.json();
      profile = await p.json();
      error = '';
    } catch (e) {
      error = `Can't reach agg serve: ${e?.message ?? e}`;
    }
  }

  onMount(() => {
    load();
    timer = setInterval(load, 5000);
  });
  onDestroy(() => clearInterval(timer));

  /** Seconds a step spent per category. */
  function split(step) {
    const by = {};
    for (const g of step.segments ?? []) by[g.cat] = (by[g.cat] ?? 0) + (g.end - g.start);
    return by;
  }

  const longest = $derived(Math.max(1, ...(profile?.steps ?? []).map((s) => s.end - s.start)));
  const mins = (secs) => (secs / 60).toFixed(1);
</script>

<div class="app">
  <a class="back" href="/">← run</a>
  <h1>Process run{profile ? ` · ${profile.project}` : ''}</h1>

  {#if error}
    <div class="card notice">{error}</div>
  {/if}

  {#if map}
    <section class="card">
      <h2>Where the run is{map.current ? ` · now ${map.current}` : map.ended ? ' · ended' : ' · stopped'}</h2>
      {#each map.phases as phase}
        <div class="phase" class:off={phase.off}>
          <span class="pn">{phase.n}</span>
          <span class="pname">{phase.name}</span>
          <span class="steps">
            {#each phase.steps as step}
              <span class="chip" data-state={step.state} title="{step.n} {step.name} · {step.state}{step.answer ? ' · ' + step.answer : ''}">{step.n}</span>
            {/each}
          </span>
        </div>
      {/each}
    </section>
  {/if}

  {#if profile && profile.steps.length}
    <section class="card">
      <h2>Where the time went · {mins(profile.end - profile.start)} min · {profile.tokens} tokens · ${profile.cost}</h2>
      <div class="legend">
        {#each CATS as c}<span class="key" data-cat={c}>{c}</span>{/each}
      </div>
      {#each profile.steps as step}
        {@const by = split(step)}
        <div class="row" title={step.rationale}>
          <span class="label mono">{step.name}</span>
          <span class="bar" style="width: {(100 * (step.end - step.start)) / longest}%">
            {#each CATS as c}
              {#if by[c]}<span class="seg" data-cat={c} style="flex: {by[c]}" title="{c} {mins(by[c])} min"></span>{/if}
            {/each}
          </span>
          <span class="mins mono">{mins(step.end - step.start)}</span>
        </div>
      {/each}
    </section>
  {/if}
</div>

<style>
  .app { max-width: 1100px; margin: 0 auto; padding: 16px; display: grid; gap: 16px; }
  .back { color: var(--ink-3); text-decoration: none; }
  h1 { font-size: 1.2rem; margin: 0; }
  h2 { font-size: 0.95rem; color: var(--ink-2); margin: 0 0 10px; }
  .phase { display: flex; gap: 10px; align-items: baseline; padding: 4px 0; border-top: 1px solid var(--border); }
  .phase.off { opacity: 0.5; }
  .pn { width: 1.6em; color: var(--ink-3); text-align: right; }
  .pname { width: 12em; flex: none; }
  .steps { display: flex; flex-wrap: wrap; gap: 4px; }
  .chip { font: 0.72rem var(--font-mono); padding: 2px 6px; border-radius: 4px; background: var(--surface-2); color: var(--ink-2); border: 1px solid var(--border); }
  .chip[data-state='done'] { background: var(--good); color: #0d1117; }
  .chip[data-state='repaired'] { background: var(--accent-dim); color: #0d1117; }
  .chip[data-state='again'] { background: var(--warning); color: #0d1117; }
  .chip[data-state='running'] { background: var(--progress); color: #0d1117; outline: 2px solid var(--accent); }
  .chip[data-state='failed'] { background: var(--critical); color: #0d1117; }
  .chip[data-state='skipped'], .chip[data-state='off'], .chip[data-state='notrun'] { opacity: 0.5; }
  .row { display: grid; grid-template-columns: 22em 1fr 4em; align-items: center; gap: 8px; padding: 2px 0; }
  .label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--ink-2); font-size: 0.78rem; }
  .bar { display: flex; height: 10px; border-radius: 3px; overflow: hidden; min-width: 2px; background: var(--progress-track); }
  .seg { display: block; }
  .mins { text-align: right; color: var(--ink-3); font-size: 0.78rem; }
  .legend { display: flex; flex-wrap: wrap; gap: 10px; margin-bottom: 8px; font-size: 0.75rem; color: var(--ink-3); }
  .key::before { content: ''; display: inline-block; width: 10px; height: 10px; margin-right: 4px; border-radius: 2px; background: var(--c); }
  [data-cat='agent'] { --c: var(--accent); }
  [data-cat='repair'] { --c: var(--serious); }
  [data-cat='critic'] { --c: #a371f7; }
  [data-cat='check'] { --c: var(--good); }
  [data-cat='hooks'] { --c: #58a6ff; }
  [data-cat='force'] { --c: var(--critical); }
  [data-cat='human'] { --c: var(--warning); }
  [data-cat='driver'] { --c: var(--muted); }
  .seg { background: var(--c); }
</style>
