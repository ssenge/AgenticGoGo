import { json } from '@sveltejs/kit';
import { rltGet } from '$lib/server/rlt.js';

// Proxy: /api/history → rlt serve /api/history
export async function GET() {
  const { status, body } = await rltGet('/api/history');
  if (status === 0) return json(body, { status: 502 });
  return json(body, { status: 200 });
}
