<script>
  import { onMount, onDestroy } from 'svelte';
  import { loop } from '$lib/loop.svelte.js';
  import Header from '$lib/components/Header.svelte';
  import Stats from '$lib/components/Stats.svelte';
  import Goals from '$lib/components/Goals.svelte';
  import Activity from '$lib/components/Activity.svelte';
  import Controls from '$lib/components/Controls.svelte';

  onMount(() => loop.start(1000));
  onDestroy(() => loop.stop());

  const s = $derived(loop.state);
  const hasState = $derived(s && !s.waiting);
</script>

<div class="app">
  <Header state={s} health={loop.health} />

  {#if loop.health.api_offline}
    <div class="card notice">
      <strong>Can't reach <code>agg serve</code>.</strong>
      Start it in your project:
      <code class="cmd">agg serve</code>
      then this page connects automatically.
    </div>
  {:else if s?.waiting || !hasState}
    <div class="card notice">
      <strong>Waiting for a run.</strong>
      <code>agg serve</code> is connected, but no loop has published state yet — start one with
      <code class="cmd">agg run</code>.
    </div>
  {/if}

  {#if hasState}
    <Stats state={s} />
    <div class="grid">
      <div class="col-main">
        <Goals state={s} />
        <Activity state={s} />
      </div>
      <div class="col-side">
        <Controls health={loop.health} />
      </div>
    </div>
  {:else}
    <!-- still show controls (disabled) so the operator sees them even before a run -->
    <div class="grid">
      <div class="col-main"></div>
      <div class="col-side"><Controls health={loop.health} /></div>
    </div>
  {/if}

  <footer>
    <span>AgenticGoGo web · polling every 1s</span>
    {#if loop.lastError}<span class="err">· {loop.lastError}</span>{/if}
  </footer>
</div>

<style>
  .app {
    max-width: 1200px;
    margin: 0 auto;
    padding: 20px;
    display: flex;
    flex-direction: column;
    gap: 16px;
    min-height: 100vh;
  }
  .notice {
    padding: 16px 20px;
    color: var(--ink-2);
    font-size: 14px;
    line-height: 1.7;
  }
  .notice strong { color: var(--ink); }
  code {
    font-family: var(--font-mono);
    background: var(--surface-2);
    padding: 1px 6px;
    border-radius: 5px;
    font-size: 13px;
  }
  .cmd { color: var(--accent); }
  .grid {
    display: grid;
    grid-template-columns: 1fr 340px;
    gap: 16px;
    align-items: start;
  }
  .col-main { display: flex; flex-direction: column; gap: 16px; min-width: 0; }
  .col-side { display: flex; flex-direction: column; gap: 16px; position: sticky; top: 20px; }
  footer {
    margin-top: auto;
    padding-top: 8px;
    font-size: 12px;
    color: var(--ink-3);
    display: flex;
    gap: 6px;
  }
  footer .err { color: var(--serious); }
  @media (max-width: 900px) {
    .grid { grid-template-columns: 1fr; }
    .col-side { position: static; }
  }
</style>
