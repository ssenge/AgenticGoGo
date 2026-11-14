import { json } from '@sveltejs/kit';
import { aggPost } from '$lib/server/agg.js';

// Proxy: /api/run → agg serve, with the query (`p`, the project) passed on.
export async function POST({ request, url }) {
  let payload;
  try {
    payload = await request.json();
  } catch {
    return json({ error: 'invalid JSON' }, { status: 400 });
  }
  const { status, body } = await aggPost(`/api/run${url.search}`, payload);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}
