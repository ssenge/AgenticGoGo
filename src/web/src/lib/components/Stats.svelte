<script>
  import { fmtTokens, fmtBytes } from '$lib/format.js';
  let { state } = $props();

  const usage = $derived(
    state?.cost_limit != null
      ? `$${(state.cost_spent ?? 0).toFixed(2)} / $${state.cost_limit.toFixed(2)}`
      : (state?.cost_spent > 0 ? `$${state.cost_spent.toFixed(2)}` : '—')
  );
</script>

<div class="tiles">
  <div class="tile card">
    <div class="label">usage <span class="note">(API-eq)</span></div>
    <div class="value mono">{usage}</div>
    <div class="sub">not a subscription charge</div>
  </div>
  <div class="tile card">
    <div class="label">tokens</div>
    <div class="value mono">{fmtTokens(state?.tokens_spent ?? 0)}</div>
    <div class="sub">{state?.budget_total ? `of ${fmtTokens(state.budget_total)} budget` : 'no budget'}</div>
  </div>
  <div class="tile card">
    <div class="label">memory</div>
    <div class="value mono">{fmtBytes(state?.memory_bytes ?? 0)}</div>
    <div class="sub">LOG.md</div>
  </div>
  <div class="tile card">
    <div class="label">phase</div>
    <div class="value mono phase">{state?.phase ?? '—'}</div>
    <div class="sub">{state?.idle_secs != null ? `idle ${state.idle_secs}s` : ''}</div>
  </div>
</div>

<style>
  .tiles { display: grid; grid-template-columns: repeat(4, 1fr); gap: 14px; }
  .tile { padding: 14px 16px; }
  .label { font-size: 12px; color: var(--ink-3); text-transform: uppercase; letter-spacing: 0.5px; }
  .note { color: var(--ink-3); text-transform: none; letter-spacing: 0; }
  .value { font-size: 24px; font-weight: 600; margin-top: 6px; color: var(--ink); }
  .value.phase { text-transform: capitalize; color: var(--accent); }
  .sub { font-size: 12px; color: var(--ink-3); margin-top: 3px; }
  @media (max-width: 720px) { .tiles { grid-template-columns: repeat(2, 1fr); } }
</style>
