import { json } from '@sveltejs/kit';
import { aggGet } from '$lib/server/agg.js';

// Proxy: /api/process?report=map|timeline|profile[&p=<project>] → agg serve /api/process (404 when the project runs no process)
export async function GET({ url }) {
  const { status, body } = await aggGet(`/api/process${url.search}`);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}
