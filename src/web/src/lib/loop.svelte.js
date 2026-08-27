// Live loop state — polls the BFF endpoints (/api/state, /api/health) on an interval and exposes
// reactive Svelte 5 state. The browser only ever talks to these SvelteKit routes, which proxy to
// `agg serve`. A single shared instance drives the whole UI.

class LoopStore {
  state = $state(null); // DashboardState | { waiting: true } | null
  health = $state({ running: false, pid: null, api_offline: true });
  lastError = $state('');
  connected = $state(false); // successfully reached the BFF at least once
  #timer = null;

  async tick() {
    try {
      const [s, h] = await Promise.all([
        fetch('/api/state').then((r) => r.json().then((b) => ({ status: r.status, b }))),
        fetch('/api/health').then((r) => r.json())
      ]);
      this.health = h;
      if (s.status === 200) this.state = s.b;
      else if (s.b?.waiting) this.state = { waiting: true };
      this.connected = true;
      this.lastError = h.api_offline ? 'agg serve is not reachable' : '';
    } catch (e) {
      this.connected = false;
      this.lastError = String(e?.message ?? e);
    }
  }

  start(intervalMs = 1000) {
    if (this.#timer) return;
    this.tick();
    this.#timer = setInterval(() => this.tick(), intervalMs);
  }

  stop() {
    if (this.#timer) clearInterval(this.#timer);
    this.#timer = null;
  }

  /** Send a control command via the BFF. Returns { ok, status, error }. */
  /// Answer an open human ask. A SEPARATE endpoint from `send`, because an answer is not a
  /// steering message: it is recorded in the ask ledger and works whether or not a workflow is
  /// running, while every `send` requires one.
  async answer(payload) {
    return this.#post('/api/answer', payload);
  }

  async send(cmd) {
    return this.#post('/api/send', cmd);
  }

  async #post(path, payload) {
    try {
      const res = await fetch(path, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      const body = await res.json().catch(() => ({}));
      // refresh immediately so the UI reflects the new state fast.
      this.tick();
      return { ok: res.ok, status: res.status, error: body?.error ?? '' };
    } catch (e) {
      return { ok: false, status: 0, error: String(e?.message ?? e) };
    }
  }
}

export const loop = new LoopStore();
