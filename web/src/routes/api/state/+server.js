import { json } from '@sveltejs/kit';
import { aggGet } from '$lib/server/agg.js';

// Proxy: /api/state → agg serve /api/state
export async function GET() {
  const { status, body } = await aggGet('/api/state');
  if (status === 0) return json(body, { status: 502 }); // agg unreachable
  if (status === 204) return json({ waiting: true }, { status: 200 });
  return json(body, { status: status === 200 ? 200 : status });
}
