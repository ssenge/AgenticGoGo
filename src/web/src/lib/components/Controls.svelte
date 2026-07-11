<script>
  import { loop } from '$lib/loop.svelte.js';
  let { health } = $props();

  const live = $derived(health.running && !health.api_offline);

  let feedback = $state(''); // transient status line
  let injectText = $state('');
  let budgetVal = $state('');
  let showInject = $state(false);
  let showBudget = $state(false);

  function flash(msg, ok = true) {
    feedback = (ok ? '✓ ' : '✕ ') + msg;
    setTimeout(() => (feedback = ''), 4000);
  }

  async function send(cmd, okMsg) {
    const r = await loop.send(cmd);
    if (r.ok) flash(okMsg);
    else if (r.status === 409) flash('no loop is running here', false);
    else flash(r.error || `failed (HTTP ${r.status})`, false);
  }

  async function doStop() {
    if (!confirm('Stop the loop after the current session? It will finish the running session, then halt.')) return;
    await send({ cmd: 'stop', reason: 'stopped from web' }, 'stop queued — the loop will halt at the session boundary');
  }
  async function doInject() {
    const text = injectText.trim();
    if (!text) return;
    await send({ cmd: 'inject', text }, 'instruction queued for the next session');
    injectText = '';
    showInject = false;
  }
  async function doBudget() {
    const raw = budgetVal.trim();
    const total = raw === '' || raw.toLowerCase() === 'none' ? null : Number(raw);
    if (total !== null && (!Number.isFinite(total) || total < 0)) {
      flash('budget must be a non-negative number (or blank for unlimited)', false);
      return;
    }
    await send({ cmd: 'budget', total }, total == null ? 'token budget set to unlimited' : `token budget set to ${total}`);
    budgetVal = '';
    showBudget = false;
  }
</script>

<section class="card panel">
  <div class="phead">
    <h2>Controls</h2>
    {#if !live}
      <span class="offhint">{health.api_offline ? 'agg serve offline' : 'no loop running'}</span>
    {/if}
  </div>

  <div class="row">
    <button onclick={() => send({ cmd: 'pause' }, 'pause queued')} disabled={!live}>⏸ Pause</button>
    <button class="primary" onclick={() => send({ cmd: 'resume' }, 'resume queued')} disabled={!live}>▶ Resume</button>
    <button onclick={() => (showInject = !showInject)} disabled={!live}>✎ Inject…</button>
    <button onclick={() => (showBudget = !showBudget)} disabled={!live}>◫ Budget…</button>
    <button class="danger" onclick={doStop} disabled={!live}>⏹ Stop</button>
  </div>

  {#if showInject}
    <div class="expand">
      <textarea bind:value={injectText} rows="2" placeholder="Instruction prepended to the next worker session, e.g. “focus on the failing edge cases first”"></textarea>
      <div class="expand-actions">
        <button onclick={() => (showInject = false)}>Cancel</button>
        <button class="primary" onclick={doInject} disabled={!injectText.trim()}>Inject</button>
      </div>
    </div>
  {/if}

  {#if showBudget}
    <div class="expand">
      <input bind:value={budgetVal} placeholder="New token ceiling (number), or blank for unlimited" />
      <div class="expand-actions">
        <button onclick={() => (showBudget = false)}>Cancel</button>
        <button class="primary" onclick={doBudget}>Set budget</button>
      </div>
    </div>
  {/if}

  {#if feedback}
    <div class="feedback" class:err={feedback.startsWith('✕')}>{feedback}</div>
  {/if}

  <p class="note">Steering applies at the next session boundary — a headless worker can't be interrupted mid-thought.</p>
</section>

<style>
  .panel { padding: 18px 20px; }
  .phead { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 12px; }
  h2 { font-size: 14px; color: var(--ink-2); text-transform: uppercase; letter-spacing: 0.6px; }
  .offhint { font-size: 12px; color: var(--ink-3); }
  .row { display: flex; gap: 10px; flex-wrap: wrap; }
  .expand { margin-top: 12px; display: flex; flex-direction: column; gap: 8px; }
  .expand-actions { display: flex; gap: 8px; justify-content: flex-end; }
  .feedback {
    margin-top: 12px; font-size: 13px; color: var(--good);
    background: color-mix(in srgb, var(--good) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--good) 30%, transparent);
    border-radius: var(--radius-sm); padding: 8px 10px;
  }
  .feedback.err {
    color: var(--critical);
    background: color-mix(in srgb, var(--critical) 12%, transparent);
    border-color: color-mix(in srgb, var(--critical) 30%, transparent);
  }
  .note { margin: 14px 0 0; font-size: 12px; color: var(--ink-3); }
</style>
