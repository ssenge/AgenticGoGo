<script>
  import Activity from '$lib/components/Activity.svelte';
  // What a running process does now: its step and agent, the current activity line and how long it has been quiet,
  // the session's recent activity, and the critics' last verdicts (they review outside the session's activity).
  let { state, reviews = [], waiting = '' } = $props();   // `waiting`: the checkpoint the run waits at
  const idle = $derived(state?.idle_secs ?? 0);
  const time = (ts) => new Date(ts * 1000).toLocaleTimeString();
</script>

<section class="card now">
  <h2>Now</h2>
  <div class="row mono">
    <span class="step">{state?.step || '—'}</span>
    <span class="who">{state?.step_agent || '—'} · {state?.step_model || state?.model || 'agent default'}</span>
  </div>
  <p class="line mono">{waiting ? `waiting for you at ${waiting}` : state?.now || 'no activity yet'}{#if !waiting && idle >= 60}<span class="idle"> · quiet for {Math.round(idle / 60)} min</span>{/if}</p>
</section>
<Activity {state} />
<section class="card reviews">
  <h2>The critics' last verdicts</h2>
  {#if reviews.length === 0}
    <p class="empty">No review yet.</p>
  {:else}
    <ul>
      {#each reviews.toReversed() as r, i (i)}
        <li><span class="mark" class:met={r.met}>{r.met ? '✓' : '✗'}</span> <span class="mono">{r.step}</span>
          <span class="ts mono">{time(r.ts)}</span><div class="why">{r.rationale}</div></li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .now, .reviews { padding: 16px 20px; }
  h2 { font-size: 14px; color: var(--ink-2); text-transform: uppercase; letter-spacing: 0.6px; margin-bottom: 8px; }
  .row { display: flex; gap: 16px; flex-wrap: wrap; font-size: 14px; }
  .who { color: var(--ink-3); }
  .line { margin-top: 6px; font-size: 13px; color: var(--ink-2); overflow-wrap: anywhere; }
  .idle { color: var(--warn, #b45309); }
  .empty { color: var(--ink-3); font-size: 14px; }
  ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
  li { font-size: 13px; }
  .mark { color: var(--bad, #b91c1c); }
  .mark.met { color: var(--good, #15803d); }
  .ts { color: var(--ink-3); margin-left: 8px; }
  .why { color: var(--ink-2); margin-top: 2px; overflow-wrap: anywhere; }
</style>
