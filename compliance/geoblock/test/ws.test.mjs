// R395-36 (Medium): WebSocket actions must be screened by the same decision as POST /v1/actions, and each
// forwarded action must carry its own fresh assertion for the node. R395-35 (Low): 'deferred' only from the edge.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createEdge, screenWsMessage, EDGE_GEO_FIELD } from '../edge/worker.mjs';
import { verifyAssertion, ReplayCache } from '../src/geo-assertion.mjs';
import { decide } from '../src/decide.mjs';
import { fixtureList, fixtureSanctions } from './helpers.mjs';

const secret = new TextEncoder().encode('postern-perps-397-test-edge-secret-00000001').subarray(0, 32);
const NOW = Date.parse('2026-11-17T01:00:00Z');
const A = '0x00000000000000000000000000000000000000a1';
const SAN = '0x983a81ca6fb1e441266d2fbcb7d8e530ac2e05a2';
let n = 0;
const ctx = (o = {}) => ({ mode: 'live', list: fixtureList(), sanctions: fixtureSanctions(), secret, now: () => NOW, randomId: () => (n++).toString(16).padStart(32, '0'), ...o });
const obs = (country, sig = {}) => ({ geo: { country, subdivision: null, trusted: true }, signals: { tor: false, vpnProxy: false, hosting: false, ...sig } });
const frame = (type, message, id = 7) => JSON.stringify({ op: 'action', id, request: { type, message, signature: '0x' + '11'.repeat(65) } });

test('R395-36: a WebSocket upgrade is never forwarded unscreened', async () => {
  const seen = [];
  const handle = createEdge({ ...ctx(), origin: 'http://origin.invalid', signalsOf: () => obs('PA').signals,
    fetchOrigin: async (r) => { seen.push(r); return new Response('x'); } });
  const up = new Request('https://perps.example/v1/ws', { headers: { upgrade: 'websocket' } });
  const res = await handle(up, { country: 'PA' });
  // Node has no WebSocketPair: the edge must refuse (fail closed), never pass the socket through raw.
  assert.equal(seen.length, 0, 'not forwarded to the origin');
  assert.equal(res.status, 503);
});

test('R395-36: action frames are decided like POST /v1/actions', () => {
  const br = screenWsMessage(frame('PlaceOrder', { account: A, reduceOnly: false }), obs('BR'), ctx());
  assert.equal(br.forward, undefined);
  const e = JSON.parse(br.reply);
  assert.deepEqual([e.op, e.id, e.error.code], ['error', 7, 'GEO_RESTRICTED']);
  const exit = screenWsMessage(frame('CancelAll', { account: A }), obs('BR'), ctx());
  assert.ok(exit.forward);
  const san = JSON.parse(screenWsMessage(frame('Withdraw', { account: A, recipient: SAN }), obs('BR'), ctx()).reply);
  assert.equal(san.error.code, 'SANCTIONED');
  assert.equal(JSON.parse(screenWsMessage('{', obs('PA'), ctx()).reply).error.code, 'MALFORMED');
  assert.equal(JSON.parse(screenWsMessage(frame('PlaceOrder', { account: A, reduceOnly: false }), obs('PA'), ctx({ secret: null })).reply).error.code, 'SEQUENCER_UNAVAILABLE');
});

test('R395-36: each forwarded action carries its own fresh assertion; client-supplied ones are refused', () => {
  const ok = screenWsMessage(frame('PlaceOrder', { account: A, reduceOnly: false }), obs('PA'), ctx());
  const f = JSON.parse(ok.forward);
  const a = f[EDGE_GEO_FIELD];
  const v = verifyAssertion(a.header, a.mac, { secret, nowMs: NOW + 1000, replay: new ReplayCache() });
  assert.equal(v.ok, true);
  assert.equal(v.geo.country, 'PA');
  const ok2 = JSON.parse(screenWsMessage(frame('PlaceOrder', { account: A, reduceOnly: false }), obs('PA'), ctx()).forward);
  assert.notEqual(ok2[EDGE_GEO_FIELD].header, a.header, 'one assertion per action, never per connection');
  const forged = JSON.parse(frame('CancelAll', { account: A })); forged[EDGE_GEO_FIELD] = a;
  assert.equal(JSON.parse(screenWsMessage(JSON.stringify(forged), obs('PA'), ctx()).reply).error.code, 'MALFORMED');
  // non-action frames pass unchanged (subscriptions are public reads)
  const sub = '{"op":"subscribe","id":1,"channel":"book","symbol":"BTC-USD"}';
  assert.equal(screenWsMessage(sub, obs('BR'), ctx()).forward, sub);
  assert.equal(JSON.parse(screenWsMessage('x'.repeat(16 * 1024 + 1), obs('PA'), ctx()).reply).error.code, 'TOO_LARGE');
});

test("R395-35: 'deferred' attestation is refused unless the caller is the edge", () => {
  const req = { mode: 'live', surface: 'api', kind: 'action', action: { type: 'PlaceOrder', reduceOnly: false },
    geo: { country: 'PA', trusted: true }, signals: { tor: false, vpnProxy: false, hosting: true },
    addresses: [{ role: 'account', address: A }], sanctions: fixtureSanctions(), nowMs: NOW, attestation: 'deferred' };
  const node = decide(fixtureList(), req);
  assert.equal(node.allow, false);
  assert.equal(node.code, 'MALFORMED');
  assert.equal(decide(fixtureList(), { ...req, caller: 'edge' }).code, 'OK');
});
