import { json } from '@sveltejs/kit';
import { rltGet, rltPost } from '$lib/server/rlt.js';

// Proxy: /api/projects → rlt serve, with the query (`p`, the project) passed on.
export async function GET({ url }) {
  const { status, body } = await rltGet(`/api/projects${url.search}`);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}

export async function POST({ request, url }) {
  let payload;
  try {
    payload = await request.json();
  } catch {
    return json({ error: 'invalid JSON' }, { status: 400 });
  }
  const { status, body } = await rltPost(`/api/projects${url.search}`, payload);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}
