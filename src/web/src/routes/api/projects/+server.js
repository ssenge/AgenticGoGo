import { json } from '@sveltejs/kit';
import { aggGet, aggPost } from '$lib/server/agg.js';

// Proxy: /api/projects → agg serve, with the query (`p`, the project) passed on.
export async function GET({ url }) {
  const { status, body } = await aggGet(`/api/projects${url.search}`);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}

export async function POST({ request, url }) {
  let payload;
  try {
    payload = await request.json();
  } catch {
    return json({ error: 'invalid JSON' }, { status: 400 });
  }
  const { status, body } = await aggPost(`/api/projects${url.search}`, payload);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}
