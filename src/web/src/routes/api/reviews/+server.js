import { json } from '@sveltejs/kit';
import { rltGet } from '$lib/server/rlt.js';

// Proxy: /api/reviews → rlt serve (the critics' last verdicts), with the query (`p`, the project) passed on.
export async function GET({ url }) {
  const { status, body } = await rltGet(`/api/reviews${url.search}`);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}
