<!-- The checkpoints waiting for a human: the document to read, the reviewers' open points, the decisions so far, and the
     answer — the humans' decisions are written into the decisions file before the yes or no. -->
<script>
  let { checkpoints, query, onchange } = $props();

  let decisions = $state({});   // per ask
  let error = $state({});
  let busy = $state({});

  async function answer(c, value) {
    if (value === 'no' && !confirm('Answer no? The run halts at this checkpoint.')) return;
    busy = { ...busy, [c.ask]: true };
    const r = await fetch(`/api/answer${query}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: c.ask, value, decisions: decisions[c.ask] ?? '', by: 'web' })
    });
    const body = await r.json().catch(() => ({}));
    error = { ...error, [c.ask]: r.ok ? '' : body.error || `failed (HTTP ${r.status})` };
    busy = { ...busy, [c.ask]: false };
    if (r.ok) decisions = { ...decisions, [c.ask]: '' };
    onchange?.();
  }
</script>

{#each checkpoints as c (c.ask)}
  <section class="card checkpoint">
    <h2>Waiting for you · {c.step}</h2>
    <p class="question">{c.question}</p>
    <details open>
      <summary>{c.doc}</summary>
      <pre>{c.document || '(the document is empty)'}</pre>
    </details>
    {#if c.open_points?.trim()}
      <details open>
        <summary>Open points · {c.open_points_file}</summary>
        <pre>{c.open_points}</pre>
      </details>
    {/if}
    {#if c.decisions?.trim()}
      <details>
        <summary>Decisions so far · {c.decisions_file}</summary>
        <pre>{c.decisions}</pre>
      </details>
    {/if}
    <label for="d-{c.ask}">Your decisions <span>one per line; added to {c.decisions_file} before the answer</span></label>
    <textarea id="d-{c.ask}" bind:value={decisions[c.ask]}></textarea>
    <div class="answer">
      <button disabled={busy[c.ask]} onclick={() => answer(c, 'yes')}>Yes, approve</button>
      <button class="no" disabled={busy[c.ask]} onclick={() => answer(c, 'no')}>No</button>
      {#if error[c.ask]}<span class="error">{error[c.ask]}</span>{/if}
    </div>
  </section>
{/each}

<style>
  .checkpoint { border-color: var(--warning); }
  .question { margin: 0 0 8px; color: var(--ink-2); }
  details { margin: 6px 0; }
  summary { cursor: pointer; color: var(--ink-3); font-size: 0.85rem; }
  pre { white-space: pre-wrap; word-break: break-word; font: 0.82rem/1.5 var(--font-mono); background: var(--surface-2);
    padding: 10px 12px; border-radius: var(--radius-sm); max-height: 60vh; overflow: auto; }
  label { display: block; margin-top: 10px; font-size: 0.85rem; }
  label span { color: var(--ink-3); margin-left: 4px; }
  textarea { width: 100%; min-height: 80px; box-sizing: border-box; font: inherit; padding: 8px; margin-top: 4px;
    background: var(--surface-2); color: var(--ink); border: 1px solid var(--border); border-radius: var(--radius-sm); }
  .answer { display: flex; gap: 8px; align-items: center; margin-top: 8px; }
  button { font: inherit; padding: 6px 14px; border-radius: var(--radius-sm); border: 0; background: var(--good); color: #0d1117; cursor: pointer; }
  button.no { background: var(--critical); }
  button:disabled { opacity: 0.5; }
  .error { color: var(--critical); font-size: 0.85rem; }
</style>
