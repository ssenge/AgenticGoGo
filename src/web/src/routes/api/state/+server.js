import { json } from '@sveltejs/kit';
import { rltGet } from '$lib/server/rlt.js';

// Proxy: /api/state → rlt serve /api/state
export async function GET({ url }) {
  const { status, body } = await rltGet(`/api/state${url.search}`);
  if (status === 0) return json(body, { status: 502 }); // rlt unreachable
  if (status === 204) return json({ waiting: true }, { status: 200 });
  return json(body, { status: status === 200 ? 200 : status });
}
