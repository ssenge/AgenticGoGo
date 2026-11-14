<script>
  import { onMount, onDestroy } from 'svelte';
  import Checkpoints from '$lib/components/process/Checkpoints.svelte';
  import Facts from '$lib/components/process/Facts.svelte';
  import Projects from '$lib/components/process/Projects.svelte';
  import RunControls from '$lib/components/process/RunControls.svelte';

  // A process run (agg process): the projects of `agg serve --projects` and the new-project form; for one project where
  // the run is (map), what waits for a human (checkpoints), the controls and where the time went (profile).
  let map = $state(null);
  let profile = $state(null);
  let checkpoints = $state([]);
  let running = $state(false);
  let projects = $state(null);   // {projects, folder} of a projects folder
  let form = $state(null);       // the process's new-project form
  let chosen = $state(null);     // the step whose details are open
  let error = $state('');
  let query = $state('');        // `?p=<project>`, passed on to every call
  let listing = $state(false);   // the projects page, not a run's
  let timer;

  const CATS = ['agent', 'repair', 'critic', 'check', 'hooks', 'force', 'human', 'driver'];
  const get = (path) => fetch(path).then(async (r) => ({ status: r.status, ok: r.ok, body: await r.json().catch(() => null) }));
  const join = (path, more) => `${path}${query ? query + '&' : '?'}${more}`;

  async function loadProjects() {
    const [list, ui] = await Promise.all([get('/api/projects'), get('/api/ui')]);
    if (list.status === 0 || list.status === 502) throw new Error('agg serve is not reachable');
    projects = list.body;
    form = ui.body?.new_project ?? null;
  }

  async function loadRun() {
    const [m, p, c, h] = await Promise.all([get(join('/api/process', 'report=map')), get(join('/api/process', 'report=profile')),
      get(`/api/checkpoint${query}`), get(`/api/health${query}`)]);
    running = !!h.body?.running;
    checkpoints = c.body?.checkpoints ?? [];
    if (m.status === 404) {
      error = 'This project has run no process yet.';
      return;
    }
    if (!m.ok || !p.ok) throw new Error('agg serve answered ' + (m.ok ? p.status : m.status));
    map = m.body;
    profile = p.body;
    error = '';
  }

  async function load() {
    try {
      if (listing) await loadProjects();
      else await loadRun();
    } catch (e) {
      error = `Can't reach agg serve: ${e?.message ?? e}`;
    }
  }

  onMount(async () => {
    const p = new URL(location.href).searchParams.get('p');
    query = p ? `?p=${encodeURIComponent(p)}` : '';
    if (!p) {
      const list = await get('/api/projects');
      listing = !!list.body?.folder;
    }
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

  // a step's facts: the map's (waivers) and its latest profile step's (what an overlay knows of a run step)
  const chosenFacts = $derived({ ...(chosen?.extra ?? {}), ...((profile?.steps ?? []).findLast((s) => s.id === chosen?.id)?.extra ?? {}) });
  const longest = $derived(Math.max(1, ...(profile?.steps ?? []).map((s) => s.end - s.start)));
  const mins = (secs) => (secs / 60).toFixed(1);
  const project = $derived(query ? decodeURIComponent(query.slice(3)) : profile?.project);
</script>

<div class="app">
  <a class="back" href={query ? '/process' : '/'}>← {query ? 'projects' : 'run'}</a>
  <h1>{listing ? 'Projects' : `Process run${project ? ` · ${project}` : ''}`}</h1>

  {#if error}
    <div class="card notice">{error}</div>
  {/if}

  {#if listing}
    {#if projects}<Projects projects={projects.projects} {form} />{/if}
  {:else}
    <section class="card"><RunControls {running} {query} onchange={load} /></section>
    <Checkpoints {checkpoints} {query} onchange={load} />
  {/if}

  {#if !listing && map}
    <section class="card">
      <h2>Where the run is{map.current ? ` · now ${map.current}` : map.ended ? ' · ended' : ' · stopped'}</h2>
      {#each map.phases as phase}
        <div class="phase" class:off={phase.off}>
          <span class="pn">{phase.n}</span>
          <span class="pname">{phase.name}</span>
          <span class="steps">
            {#each phase.steps as step}
              <button class="chip" data-state={step.state} class:chosen={chosen?.id === step.id} onclick={() => (chosen = chosen?.id === step.id ? null : step)}
                title="{step.n} {step.name} · {step.state}{step.answer ? ' · ' + step.answer : ''}">{step.n}</button>
            {/each}
          </span>
        </div>
      {/each}
      {#if chosen}
        <div class="detail">
          <strong>{chosen.n} {chosen.name}</strong> · {chosen.state}{chosen.answer ? ` · ${chosen.answer}` : ''}
          · runs {chosen.runs ?? 0} · repairs {chosen.repairs ?? 0}
          <Facts facts={chosenFacts} />
        </div>
      {/if}
      {#if map.extra}<Facts facts={map.extra} />{/if}
      {#if map.overlay_error}<p class="overlay-error">{map.overlay_error}</p>{/if}
    </section>
  {/if}

  {#if !listing && profile && profile.steps.length}
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
      {#if profile.extra}<Facts facts={profile.extra} />{/if}
    </section>
  {/if}
</div>

<style>
  .app { max-width: 1100px; margin: 0 auto; padding: 16px; display: grid; gap: 16px; }
  .back { color: var(--ink-3); text-decoration: none; }
  h1 { font-size: 1.2rem; margin: 0; }
  .app :global(.card) { padding: 14px 18px; }
  .app :global(h2) { font-size: 0.95rem; color: var(--ink-2); margin: 0 0 10px; }
  .phase { display: flex; gap: 10px; align-items: baseline; padding: 4px 0; border-top: 1px solid var(--border); }
  .phase.off { opacity: 0.5; }
  .pn { width: 1.6em; color: var(--ink-3); text-align: right; }
  .pname { width: 12em; flex: none; }
  .steps { display: flex; flex-wrap: wrap; gap: 4px; }
  .chip { cursor: pointer; font: 0.72rem var(--font-mono); padding: 2px 6px; border-radius: 4px; background: var(--surface-2); color: var(--ink-2); border: 1px solid var(--border); }
  .chip[data-state='done'] { background: var(--good); color: #0d1117; }
  .chip[data-state='repaired'] { background: var(--accent-dim); color: #0d1117; }
  .chip[data-state='again'] { background: var(--warning); color: #0d1117; }
  .chip[data-state='running'] { background: var(--progress); color: #0d1117; outline: 2px solid var(--accent); }
  .chip[data-state='failed'] { background: var(--critical); color: #0d1117; }
  .chip[data-state='skipped'], .chip[data-state='off'], .chip[data-state='notrun'] { opacity: 0.5; }
  .chip.chosen { outline: 2px solid var(--ink-2); }
  .detail { margin-top: 10px; padding-top: 8px; border-top: 1px solid var(--border); font-size: 0.85rem; }
  .overlay-error { color: var(--critical); font-size: 0.8rem; }
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
