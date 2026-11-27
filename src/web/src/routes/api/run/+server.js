import { json } from '@sveltejs/kit';
import { rltPost } from '$lib/server/rlt.js';

// Proxy: /api/run → rlt serve, with the query (`p`, the project) passed on.
export async function POST({ request, url }) {
  let payload;
  try {
    payload = await request.json();
  } catch {
    return json({ error: 'invalid JSON' }, { status: 400 });
  }
  const { status, body } = await rltPost(`/api/run${url.search}`, payload);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}
