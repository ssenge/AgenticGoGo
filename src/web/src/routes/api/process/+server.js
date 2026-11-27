import { json } from '@sveltejs/kit';
import { rltGet } from '$lib/server/rlt.js';

// Proxy: /api/process?report=map|timeline|profile[&p=<project>] → rlt serve /api/process (404 when the project runs no process)
export async function GET({ url }) {
  const { status, body } = await rltGet(`/api/process${url.search}`);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}
