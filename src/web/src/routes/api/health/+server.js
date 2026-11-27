import { json } from '@sveltejs/kit';
import { rltGet } from '$lib/server/rlt.js';

// Proxy: /api/health → rlt serve /api/health. When rlt is unreachable, report a distinct
// api_offline state so the UI can tell "no loop" apart from "can't reach rlt".
export async function GET({ url }) {
  const { status, body } = await rltGet(`/api/health${url.search}`);
  if (status === 0) return json({ running: false, pid: null, api_offline: true }, { status: 200 });
  return json({ ...body, api_offline: false }, { status: 200 });
}
