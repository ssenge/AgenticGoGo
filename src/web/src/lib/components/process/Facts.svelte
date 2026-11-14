<!-- A process's own facts (its `ui.overlay`): generic key and value rows; agg knows nothing of what they mean. -->
<script>
  let { facts } = $props();

  /** A value as text: a list one line per item, an object as JSON. */
  function text(v) {
    if (v === null || v === undefined) return '—';
    if (Array.isArray(v)) return v.map((x) => (typeof x === 'object' ? JSON.stringify(x) : String(x))).join('\n');
    if (typeof v === 'object') return JSON.stringify(v, null, 1);
    return String(v);
  }

  const rows = $derived(Object.entries(facts ?? {}));
</script>

{#if rows.length}
  <dl class="facts">
    {#each rows as [key, value]}
      <dt>{key}</dt>
      <dd>{text(value)}</dd>
    {/each}
  </dl>
{/if}

<style>
  .facts { display: grid; grid-template-columns: minmax(8em, max-content) 1fr; gap: 4px 12px; margin: 6px 0 0; font-size: 0.8rem; }
  dt { color: var(--ink-3); }
  dd { margin: 0; white-space: pre-wrap; word-break: break-word; color: var(--ink-2); }
</style>
