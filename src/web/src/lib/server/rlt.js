// Server-side (BFF) client for the `rlt serve` API. The browser NEVER calls rlt directly — it
// calls these SvelteKit endpoints, which proxy to rlt. This keeps rlt's API off the public
// internet and gives us one place for auth/secrets in the remote phase.
//
// Config via env:
//   RLT_API   base URL of `rlt serve`   (default http://127.0.0.1:7878)
//   RLT_TOKEN bearer token, if rlt serve was started with --token (remote phase; empty locally)

const BASE = (process.env.RLT_API || 'http://127.0.0.1:7878').replace(/\/$/, '');
const TOKEN = process.env.RLT_TOKEN || '';

function headers(extra = {}) {
  const h = { ...extra };
  if (TOKEN) h['Authorization'] = `Bearer ${TOKEN}`;
  return h;
}

/** GET an rlt endpoint, returning { status, body }. body is parsed JSON or null. */
export async function rltGet(path) {
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
    // rlt serve unreachable — surface as a distinct state so the UI shows "API offline".
    return { status: 0, body: { error: `rlt serve unreachable at ${BASE}: ${e?.message ?? e}` } };
  }
}

/** POST a bus command. Returns { status, body }. */
export async function rltSend(cmd) {
  return rltPost('/api/send', cmd);
}

/**
 * POST an answer to an open human ask. A SEPARATE rlt endpoint from /api/send, because an answer is
 * not a steering message: it is recorded in the ask ledger and works whether or not a workflow is
 * running, while every send requires one.
 */
export async function rltAnswer(payload, search = '') {
  return rltPost(`/api/answer${search}`, payload);
}

/** POST to an rlt endpoint, returning { status, body }. */
export async function rltPost(path, payload) {
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
    return { status: 0, body: { error: `rlt serve unreachable: ${e?.message ?? e}` } };
  }
}
