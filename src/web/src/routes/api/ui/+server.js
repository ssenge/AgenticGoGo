import { json } from '@sveltejs/kit';
import { rltGet } from '$lib/server/rlt.js';

// Proxy: /api/ui → rlt serve, with the query (`p`, the project) passed on.
export async function GET({ url }) {
  const { status, body } = await rltGet(`/api/ui${url.search}`);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}

