<script>
  import { fmtTokens, fmtMoney, agentKey } from '$lib/format.js';
  let { state } = $props();

  // §7.4: a mixed run's aggregate tokens/cost are meaningless without a per-agent split. `per_agent`
  // sums to tokens_spent/cost_spent; empty ⇒ a single-agent run (fall back to a one-line aggregate).
  const entries = $derived(Object.entries(state?.per_agent ?? {}));
  // total cost is null (→ "—") unless SOME agent could report a price — never a lying "$0.00".
  const anyCost = $derived(entries.some(([, u]) => u?.cost != null) || (state?.cost_spent ?? 0) > 0);
  const totalCost = $derived(anyCost ? (state?.cost_spent ?? 0) : null);
</script>

<section class="card panel">
  <div class="phead">
    <h2>Per-agent</h2>
    <div class="cols mono"><span>tokens</span><span>usage</span></div>
  </div>

  {#if entries.length === 0}
    <ul class="rows">
      <li>
        <span class="agent">all</span>
        <span class="tok mono">{fmtTokens(state?.tokens_spent ?? 0)}</span>
        <span class="cost mono">{fmtMoney(totalCost)}</span>
      </li>
    </ul>
    <div class="note">single agent — no per-agent split</div>
  {:else}
    <ul class="rows">
      {#each entries as [agent, u] (agent)}
        <li>
          <span class="agent"><span class="adot" data-a={agentKey(agent)}></span>{agent}</span>
          <span class="tok mono">{fmtTokens(u?.tokens ?? 0)}</span>
          <span class="cost mono">{fmtMoney(u?.cost)}</span>
        </li>
      {/each}
      <li class="total">
        <span class="agent">total</span>
        <span class="tok mono">{fmtTokens(state?.tokens_spent ?? 0)}</span>
        <span class="cost mono">{fmtMoney(totalCost)}</span>
      </li>
    </ul>
    <div class="note">usage is API-equivalent, not a subscription charge</div>
  {/if}
</section>

<style>
  .panel { padding: 16px 18px; }
  .phead { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 8px; }
  h2 { font-size: 14px; color: var(--ink-2); text-transform: uppercase; letter-spacing: 0.6px; }
  .cols { display: grid; grid-template-columns: 74px 62px; gap: 10px; font-size: 11px; color: var(--ink-3); text-align: right; }

  .rows { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; }
  li {
    display: grid;
    grid-template-columns: 1fr 74px 62px;
    align-items: center;
    gap: 10px;
    padding: 8px 0;
    border-top: 1px solid var(--border);
    font-size: 14px;
  }
  li:first-child { border-top: none; }
  .agent { display: flex; align-items: center; gap: 8px; color: var(--ink); overflow: hidden; text-overflow: ellipsis; }
  .tok { text-align: right; color: var(--ink); }
  .cost { text-align: right; color: var(--ink-2); }

  .total { border-top: 1px solid var(--border-strong); font-weight: 600; }
  .total .cost { color: var(--ink); }

  .adot { width: 8px; height: 8px; border-radius: 50%; flex: none; background: var(--accent); }
  .adot[data-a='claude'] { background: #a371f7; }
  .adot[data-a='codex'] { background: var(--good); }
  .adot[data-a='copilot'] { background: #539bf5; }
  .adot[data-a='other'] { background: var(--accent); }

  .note { font-size: 12px; color: var(--ink-3); margin-top: 8px; }
</style>
