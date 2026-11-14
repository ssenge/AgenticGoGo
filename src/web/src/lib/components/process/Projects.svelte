<!-- The projects of `agg serve --projects`: whether each runs and what waits, and the process's new-project form. -->
<script>
  let { projects, form } = $props();

  let values = $state({ name: '' });
  let message = $state('');
  let busy = $state(false);

  async function create(e) {
    e.preventDefault();
    busy = true;
    message = 'Making the project and starting its run…';
    const r = await fetch('/api/projects', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(values)
    });
    const body = await r.json().catch(() => ({}));
    busy = false;
    if (r.ok) location.href = `/process?p=${encodeURIComponent(body.project)}`;
    else message = body.error || `the run didn't start (HTTP ${r.status})`;
  }
</script>

<section class="card">
  <h2>Projects</h2>
  {#if projects.length}
    <table>
      <tbody>
        {#each projects as p}
          <tr>
            <td><a href="/process?p={encodeURIComponent(p.name)}">{p.name}</a></td>
            <td>{p.running ? 'running' : 'not running'}{p.asks ? ` · ${p.asks} waiting for you` : ''}</td>
            <td class="mono">{p.step ?? ''}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
    <p class="none">No project has run a process yet.</p>
  {/if}
</section>

{#if form}
  <section class="card">
    <h2>Start a new project</h2>
    <form onsubmit={create}>
      <label for="f-name">Project name <span>lowercase letters, digits and -</span></label>
      <input id="f-name" required pattern="[a-z0-9][a-z0-9\-]{'{0,40}'}" bind:value={values.name} />
      {#each form.fields as f}
        <label for="f-{f.name}">{f.label}{#if f.hint} <span>{f.hint}</span>{/if}</label>
        {#if f.kind === 'textarea'}
          <textarea id="f-{f.name}" required={f.required} bind:value={values[f.name]}></textarea>
        {:else if f.kind === 'select'}
          <select id="f-{f.name}" required={f.required} bind:value={values[f.name]}>
            {#each f.options as o}<option value={o}>{o}</option>{/each}
          </select>
        {:else}
          <input id="f-{f.name}" required={f.required} bind:value={values[f.name]} />
        {/if}
      {/each}
      <button type="submit" disabled={busy}>Start the run</button>
      {#if message}<p class="msg">{message}</p>{/if}
    </form>
  </section>
{/if}

<style>
  table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
  td { padding: 6px 4px; border-top: 1px solid var(--border); }
  a { color: var(--accent); }
  .none, .msg { color: var(--ink-3); }
  label { display: block; margin: 10px 0 4px; font-size: 0.85rem; }
  label span { color: var(--ink-3); margin-left: 4px; }
  input, textarea, select { width: 100%; box-sizing: border-box; font: inherit; padding: 7px 9px; background: var(--surface-2);
    color: var(--ink); border: 1px solid var(--border); border-radius: var(--radius-sm); }
  textarea { min-height: 100px; }
  button { margin-top: 14px; font: inherit; padding: 8px 16px; border-radius: var(--radius-sm); border: 0; background: var(--accent); color: #0d1117; cursor: pointer; }
</style>
