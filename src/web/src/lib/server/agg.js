// Server-side (BFF) client for the `agg serve` API. The browser NEVER calls agg directly — it
// calls these SvelteKit endpoints, which proxy to agg. This keeps agg's API off the public
// internet and gives us one place for auth/secrets in the remote phase.
//
// Config via env:
//   AGG_API   base URL of `agg serve`   (default http://127.0.0.1:7878)
//   AGG_TOKEN bearer token, if agg serve was started with --token (remote phase; empty locally)

const BASE = (process.env.AGG_API || 'http://127.0.0.1:7878').replace(/\/$/, '');
const TOKEN = process.env.AGG_TOKEN || '';

function headers(extra = {}) {
  const h = { ...extra };
  if (TOKEN) h['Authorization'] = `Bearer ${TOKEN}`;
  return h;
}

/** GET an agg endpoint, returning { status, body }. body is parsed JSON or null. */
export async function aggGet(path) {
  try {
    const res = await fetch(`${BASE}${path}`, { headers: headers() });
    // 204 = no snapshot yet (loop hasn't published)
    if (res.status === 204) return { status: 204, body: { waiting: true } };
    const text = await res.text();
    let body = null;
    try {
      body = text ? JSON.parse(text) : null;
    } catch {
      body = null;
    }
    return { status: res.status, body };
  } catch (e) {
    // agg serve unreachable — surface as a distinct state so the UI shows "API offline".
    return { status: 0, body: { error: `agg serve unreachable at ${BASE}: ${e?.message ?? e}` } };
  }
}

/** POST a bus command. Returns { status, body }. */
export async function aggSend(cmd) {
  return aggPost('/api/send', cmd);
}

/**
 * POST an answer to an open human ask. A SEPARATE agg endpoint from /api/send, because an answer is
 * not a steering message: it is recorded in the ask ledger and works whether or not a workflow is
 * running, while every send requires one.
 */
export async function aggAnswer(payload) {
  return aggPost('/api/answer', payload);
}

/** POST to an agg endpoint, returning { status, body }. */
async function aggPost(path, payload) {
  try {
    const res = await fetch(`${BASE}${path}`, {
      method: 'POST',
      headers: headers({ 'Content-Type': 'application/json' }),
      body: JSON.stringify(payload)
    });
    const text = await res.text();
    let body = null;
    try {
      body = text ? JSON.parse(text) : null;
    } catch {
      body = null;
    }
    return { status: res.status, body };
  } catch (e) {
    return { status: 0, body: { error: `agg serve unreachable: ${e?.message ?? e}` } };
  }
}
