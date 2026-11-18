// The busy-market scenario (391's request: >= 300 book/trade messages per second). Wall-clock dependent: under
// heavy load rerun alone before calling a failure (BRIEF-ROUND18 rule 5).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { startMock } from '../server.mjs';

test('busy=1 delivers at least 300 book and trade messages per second', { timeout: 60_000 }, async () => {
  const m = await startMock({ port: 0, rateLimit: false, blocks: 5 });
  try {
    await fetch(m.url + '/mock/scenario?busy=1');
    const ws = new WebSocket(m.url.replace('http', 'ws') + '/v1/ws');
    let n = 0;
    ws.onmessage = () => n++;
    await new Promise((r) => (ws.onopen = r));
    for (const s of ['BTC-USD', 'ETH-USD', 'SOL-USD']) for (const ch of ['book', 'trades']) ws.send(JSON.stringify({ op: 'subscribe', channel: ch, symbol: s }));
    await new Promise((r) => setTimeout(r, 500));
    n = 0;
    const t0 = performance.now();
    await new Promise((r) => setTimeout(r, 2000));
    const rate = n / ((performance.now() - t0) / 1000);
    console.log(`busy: ${Math.round(rate)} messages/s`);
    ws.close();
    assert.ok(rate >= 300, `${rate} messages/s`);
  } finally {
    await m.close();
  }
});
