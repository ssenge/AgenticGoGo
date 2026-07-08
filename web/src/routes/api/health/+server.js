import { json } from '@sveltejs/kit';
import { aggGet } from '$lib/server/agg.js';

// Proxy: /api/health → agg serve /api/health. When agg is unreachable, report a distinct
// api_offline state so the UI can tell "no loop" apart from "can't reach agg".
export async function GET() {
  const { status, body } = await aggGet('/api/health');
  if (status === 0) return json({ running: false, pid: null, api_offline: true }, { status: 200 });
  return json({ ...body, api_offline: false }, { status: 200 });
}
