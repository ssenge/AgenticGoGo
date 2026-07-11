import { json } from '@sveltejs/kit';
import { aggSend } from '$lib/server/agg.js';

// Proxy: POST /api/send → agg serve POST /api/send. The browser posts a control command here;
// this forwards it to agg (which applies the liveness guard and returns 409 if no loop is live).
export async function POST({ request }) {
  let cmd;
  try {
    cmd = await request.json();
  } catch {
    return json({ error: 'invalid JSON' }, { status: 400 });
  }
  const { status, body } = await aggSend(cmd);
  if (status === 0) return json(body, { status: 502 });
  return json(body ?? {}, { status });
}
