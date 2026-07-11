<script>
  let { state } = $props();
  const events = $derived(state?.recent ?? []);

  function icon(kind) {
    switch (kind) {
      case 'tool': return '🔧';
      case 'tool_result': return '↳';
      case 'think': return '💬';
      case 'result': return '✓';
      case 'init': return '▸';
      default: return '·';
    }
  }
</script>

<section class="card panel">
  <div class="phead">
    <h2>Activity</h2>
    <span class="live mono">{events.length} events</span>
  </div>
  {#if events.length === 0}
    <div class="empty">Waiting for worker activity…</div>
  {:else}
    <ul class="feed">
      {#each events as e, i (i)}
        <li>
          <span class="ts mono">{e.ts}</span>
          <span class="ico">{icon(e.kind)}</span>
          <span class="txt mono" class:think={e.kind === 'think'}>{e.text}</span>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .panel { padding: 18px 20px; display: flex; flex-direction: column; min-height: 0; }
  .phead { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 10px; }
  h2 { font-size: 14px; color: var(--ink-2); text-transform: uppercase; letter-spacing: 0.6px; }
  .live { font-size: 13px; color: var(--ink-3); }
  .empty { color: var(--ink-3); font-size: 14px; }
  .feed {
    list-style: none; margin: 0; padding: 0;
    display: flex; flex-direction: column; gap: 2px;
    overflow-y: auto;
    max-height: 340px;
  }
  li { display: grid; grid-template-columns: 66px 20px 1fr; gap: 8px; align-items: baseline; padding: 3px 0; font-size: 13px; }
  .ts { color: var(--ink-3); }
  .ico { text-align: center; }
  .txt { color: var(--ink-2); overflow-wrap: anywhere; }
  .txt.think { color: var(--accent); }
</style>
