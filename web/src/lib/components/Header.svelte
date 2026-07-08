<script>
  import { phaseStatus, fmtDur, fmtTokens } from '$lib/format.js';
  let { state, health } = $props();

  const dotColor = $derived(
    health.api_offline ? 'muted' : health.running ? phaseStatus(state?.phase) : 'muted'
  );
  const statusText = $derived(
    health.api_offline ? 'agg serve offline' : health.running ? (state?.phase ?? 'running') : 'idle'
  );
</script>

<header class="hdr card">
  <div class="left">
    <div class="brand">
      <span class="logo mono">agg</span>
      <span class="name">AgenticGoGo</span>
    </div>
    {#if state && !state.waiting}
      <div class="proj mono">{state.project}</div>
    {/if}
  </div>

  <div class="meta">
    {#if state && !state.waiting}
      <span class="m"><span class="k">model</span> <span class="v mono">{state.model}</span></span>
      <span class="m"><span class="k">session</span> <span class="v mono">#{state.session}{state.lifetime_session > state.session ? ` of ${state.lifetime_session}` : ''}</span></span>
      <span class="m"><span class="k">up</span> <span class="v mono">{fmtDur(state.up_secs)}</span></span>
      <span class="m"><span class="k">tokens</span> <span class="v mono">{fmtTokens(state.tokens_spent)}{state.budget_total ? ` / ${fmtTokens(state.budget_total)}` : ''}</span></span>
    {/if}
    <span class="status" data-c={dotColor}>
      <span class="dot" class:pulse={health.running && !health.api_offline}></span>
      {statusText}
    </span>
  </div>
</header>

<style>
  .hdr {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 14px 20px;
    flex-wrap: wrap;
  }
  .left { display: flex; align-items: center; gap: 16px; }
  .brand { display: flex; align-items: baseline; gap: 10px; }
  .logo {
    background: var(--accent-dim);
    color: #fff;
    padding: 2px 8px;
    border-radius: 6px;
    font-weight: 700;
    font-size: 15px;
  }
  .name { font-size: 17px; font-weight: 600; letter-spacing: 0.2px; }
  .proj { color: var(--ink-2); font-size: 14px; }
  .meta { display: flex; align-items: center; gap: 18px; flex-wrap: wrap; }
  .m { display: flex; gap: 6px; align-items: baseline; font-size: 13px; }
  .k { color: var(--ink-3); }
  .v { color: var(--ink); }
  .status { display: flex; align-items: center; gap: 7px; font-size: 13px; color: var(--ink-2); }
  .dot {
    width: 9px; height: 9px; border-radius: 50%;
    background: var(--muted);
  }
  .status[data-c='good'] .dot { background: var(--good); }
  .status[data-c='accent'] .dot { background: var(--accent); }
  .status[data-c='serious'] .dot { background: var(--serious); }
  .status[data-c='muted'] .dot { background: var(--muted); }
  .pulse { animation: pulse 1.6s ease-in-out infinite; }
  @keyframes pulse {
    0%, 100% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--good) 60%, transparent); }
    50% { box-shadow: 0 0 0 5px color-mix(in srgb, var(--good) 0%, transparent); }
  }
</style>
