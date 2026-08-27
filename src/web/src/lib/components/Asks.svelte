<!-- OPEN HUMAN ASKS — the reply channel for a loop that is waiting on a person.
     A `hil_*` call blocks with NO TIMEOUT, so a run waiting here waits indefinitely: this panel is
     most of the mitigation for "somebody forgot to answer". It therefore renders ABOVE everything
     else on the page and leads with the age, not the question — a blocked loop outranks a score.
     The ask arrives via `state.json.asks` (polled with the rest of the state); the answer goes back
     through POST /api/send, validated server-side against the recorded options. -->
<script>
  import { loop } from '$lib/loop.svelte.js';

  // ⚠ Destructured as `snap`, NOT `state`. A local binding called `state` SHADOWS the `$state` rune:
  // Svelte then reads `$state(...)` as a store subscription of `state` invoked as a function, which
  // compiles with only a WARNING and throws at runtime — taking every sibling rendered after this
  // component with it. Caught by the browser e2e, which is the only thing that runs the built page.
  let { state: snap } = $props();

  const asks = $derived(snap?.asks ?? []);

  let inputs = $state({});   // per-ask free-text, keyed by id
  let busy = $state({});     // per-ask in-flight guard: an ask must not be answered twice
  let error = $state({});

  function age(secs) {
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.floor(secs / 60)}m`;
    const h = Math.floor(secs / 3600);
    return `${h}h ${Math.floor((secs % 3600) / 60)}m`;
  }

  async function answer(id, value) {
    if (!value || busy[id]) return;
    busy = { ...busy, [id]: true };
    error = { ...error, [id]: '' };
    const r = await loop.send({ cmd: 'answer', id, value, by: 'web' });
    if (!r.ok) {
      // The server refuses a value that is not on the recorded option list and leaves the ask OPEN,
      // so surfacing the message verbatim is what tells the operator what it WOULD accept.
      error = { ...error, [id]: r.error || `failed (HTTP ${r.status})` };
      busy = { ...busy, [id]: false };
      return;
    }
    // Deliberately NOT removed here. The ask disappears when the loop records the answer and
    // republishes `state.json` — so what you see is what the run actually consumed, never an
    // optimistic guess that could disagree with it.
    inputs = { ...inputs, [id]: '' };
  }
</script>

{#if asks.length > 0}
  <section class="card asks">
    <h2>⏳ Waiting on you<span class="count">{asks.length}</span></h2>

    {#each asks as a (a.id)}
      <article class="ask">
        <header>
          <span class="age" title="how long this has been waiting">{age(a.age_secs)}</span>
          <span class="mono id">{a.id}</span>
          <span class="origin" title={a.origin === 'worker'
            ? 'the worker asked and ended its session — the loop is NOT blocked'
            : 'the driver is blocked on this answer'}>{a.origin}</span>
          {#if a.origin === 'driver'}<span class="blocking">blocking</span>{/if}
        </header>

        <p class="q">{a.question}</p>

        {#if a.options?.length}
          <div class="opts">
            {#each a.options as o}
              <button disabled={busy[a.id]} onclick={() => answer(a.id, o)}>{o}</button>
            {/each}
          </div>
        {:else}
          <form class="free" onsubmit={(e) => { e.preventDefault(); answer(a.id, inputs[a.id]); }}>
            <input
              type="text"
              placeholder="your answer…"
              bind:value={inputs[a.id]}
              disabled={busy[a.id]}
            />
            <button class="primary" type="submit" disabled={busy[a.id] || !inputs[a.id]}>Answer</button>
          </form>
          <!-- The one rule a UI must not let you break: an answer is written to the ask ledger and
               into the worker's next brief, both files on disk. -->
          <p class="warn">Never paste a secret here — name where it was placed instead.</p>
        {/if}

        {#if error[a.id]}<p class="err">{error[a.id]}</p>{/if}
      </article>
    {/each}
  </section>
{/if}

<style>
  .asks { border-color: var(--warn, #c9a227); }
  h2 { display: flex; align-items: center; gap: .5rem; margin: 0 0 .75rem; font-size: 1rem; }
  .count {
    background: var(--warn, #c9a227); color: #000; border-radius: 999px;
    padding: 0 .5rem; font-size: .75rem; font-weight: 700;
  }
  .ask + .ask { border-top: 1px solid var(--border, #333); margin-top: .75rem; padding-top: .75rem; }
  header { display: flex; align-items: center; gap: .5rem; font-size: .75rem; opacity: .8; }
  .age { font-weight: 700; opacity: 1; }
  .origin { text-transform: uppercase; letter-spacing: .04em; }
  .blocking {
    background: var(--warn, #c9a227); color: #000; border-radius: 3px;
    padding: 0 .35rem; font-weight: 700;
  }
  .q { margin: .4rem 0 .6rem; }
  .opts { display: flex; flex-wrap: wrap; gap: .5rem; }
  .free { display: flex; gap: .5rem; }
  .free input { flex: 1; min-width: 0; }
  .warn { font-size: .7rem; opacity: .65; margin: .4rem 0 0; }
  .err { color: var(--bad, #e5534b); font-size: .8rem; margin: .4rem 0 0; }
  /* the panel is the point on a phone — never let the buttons shrink out of reach */
  @media (max-width: 560px) {
    .opts button, .free button { min-height: 2.5rem; }
  }
</style>
