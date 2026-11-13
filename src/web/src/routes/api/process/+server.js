import { json } from '@sveltejs/kit';
import { aggGet } from '$lib/server/agg.js';

// Proxy: /api/process?report=map|timeline|profile → agg serve /api/process (404 when the project runs no process)
export async function GET({ url }) {
  const report = url.searchParams.get('report') ?? 'map';
  const { status, body } = await aggGet(`/api/process?report=${encodeURIComponent(report)}`);
  return json(body ?? {}, { status: status === 0 ? 502 : status });
}
