import { json } from '@sveltejs/kit';
import { aggGet } from '$lib/server/agg.js';

// Proxy: /api/checkpoint → agg serve, with the query (`p`, the project) passed on.
export async function GET({ url }) {
  const { status, body } = await aggGet(`/api/checkpoint${url.search}`);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}

