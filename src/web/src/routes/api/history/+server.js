import { json } from '@sveltejs/kit';
import { aggGet } from '$lib/server/agg.js';

// Proxy: /api/history → agg serve /api/history
export async function GET() {
  const { status, body } = await aggGet('/api/history');
  if (status === 0) return json(body, { status: 502 });
  return json(body, { status: 200 });
}
