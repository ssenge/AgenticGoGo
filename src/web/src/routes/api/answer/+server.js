import { json } from '@sveltejs/kit';
import { aggAnswer } from '$lib/server/agg.js';

// Proxy: POST /api/answer → agg serve POST /api/answer. Separate from /api/send on purpose: an
// answer is a durable fact recorded in the ask ledger, not a steering message, so it does NOT
// require a running workflow — while every /api/send does. agg validates the value against the
// ask's recorded options and returns 400 with the acceptable ones if it is off the list.
export async function POST({ request }) {
  let payload;
  try {
    payload = await request.json();
  } catch {
    return json({ error: 'invalid JSON' }, { status: 400 });
  }
  const { status, body } = await aggAnswer(payload);
  if (status === 0) return json(body, { status: 502 });
  return json(body ?? {}, { status });
}
