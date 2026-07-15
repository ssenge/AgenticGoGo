<script>
  import { phaseStatus } from '$lib/format.js';
  let { state } = $props();

  // §7.4: a mixed claude/codex run is uninterpretable without knowing who ran THIS step. `step_model`
  // falls back to the worker-default `model` so an older state.json (no per-step model) still shows.
  const step = $derived(state?.step || '—');
  const agent = $derived(state?.step_agent || '—');
  const model = $derived(state?.step_model || state?.model || '—');
  const phase = $derived(state?.phase || '—');
</script>

<section class="card step">
  <div class="lead">
    <span class="klabel">current step</span>
    <div class="row">
      <span class="name mono">{step}</span>
      <span class="am mono">
        <span class="agent">{agent}</span><span class="mid">·</span><span class="model">{model}</span>
      </span>
    </div>
  </div>
  <span class="phase" data-c={phaseStatus(phase)}>{phase}</span>
</section>

<style>
  .step {
    display: flex; align-items: center; justify-content: space-between; gap: 16px;
    padding: 16px 20px; flex-wrap: wrap;
  }
  .lead { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .klabel { font-size: 12px; color: var(--ink-3); text-transform: uppercase; letter-spacing: 0.6px; }
  .row { display: flex; align-items: baseline; gap: 14px; flex-wrap: wrap; min-width: 0; }
  .name { font-size: 22px; font-weight: 600; color: var(--accent); }
  .am { font-size: 14px; color: var(--ink-2); display: flex; align-items: baseline; gap: 8px; }
  .agent { color: var(--ink); font-weight: 500; }
  .mid { color: var(--ink-3); }
  .model { color: var(--ink-2); }

  .phase {
    font-size: 13px; font-weight: 600; text-transform: capitalize;
    padding: 4px 12px; border-radius: 999px;
    border: 1px solid var(--border-strong);
  }
  .phase[data-c='good'] { color: var(--good); border-color: color-mix(in srgb, var(--good) 45%, transparent); }
  .phase[data-c='accent'] { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 45%, transparent); }
  .phase[data-c='serious'] { color: var(--serious); border-color: color-mix(in srgb, var(--serious) 45%, transparent); }
  .phase[data-c='muted'] { color: var(--ink-3); }
</style>
