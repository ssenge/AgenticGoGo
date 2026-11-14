<!-- Start, resume and stop a process run: what `agg process <file> <project> [--resume]` and a SIGTERM do. -->
<script>
  let { running, query, onchange } = $props();

  let message = $state('');
  let busy = $state(false);

  async function act(action, ask) {
    if (ask && !confirm(ask)) return;
    busy = true;
    const r = await fetch(`/api/run${query}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ action })
    });
    const body = await r.json().catch(() => ({}));
    message = r.ok ? `${action}: done` : body.error || `${action} failed (HTTP ${r.status})`;
    busy = false;
    onchange?.();
  }
</script>

<div class="controls">
  <span class="state" data-running={running}>{running ? 'running' : 'not running'}</span>
  {#if running}
    <button disabled={busy} onclick={() => act('stop', 'Stop the run? Its session is interrupted; Resume continues where it stopped.')}>Stop</button>
  {:else}
    <button disabled={busy} onclick={() => act('resume')}>Resume</button>
    <button class="quiet" disabled={busy} onclick={() => act('start', 'Start the process again from its first step?')}>Start fresh</button>
  {/if}
  {#if message}<span class="msg">{message}</span>{/if}
</div>

<style>
  .controls { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .state { font-size: 0.8rem; padding: 2px 8px; border-radius: 999px; background: var(--surface-2); color: var(--ink-3); }
  .state[data-running='true'] { background: var(--progress); color: #0d1117; }
  button { font: inherit; padding: 6px 14px; border-radius: var(--radius-sm); border: 0; background: var(--accent); color: #0d1117; cursor: pointer; }
  button.quiet { background: var(--surface-2); color: var(--ink-2); border: 1px solid var(--border); }
  button:disabled { opacity: 0.5; cursor: default; }
  .msg { font-size: 0.8rem; color: var(--ink-3); }
</style>
