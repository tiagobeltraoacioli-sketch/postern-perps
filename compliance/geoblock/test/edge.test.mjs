import test from 'node:test';
import assert from 'node:assert/strict';
import { createEdge, MAX_BODY_BYTES } from '../edge/worker.mjs';
import worker from '../edge/worker.mjs';
import { verifyAssertion, ReplayCache, HEADER, MAC_HEADER } from '../src/geo-assertion.mjs';
import { decide } from '../src/decide.mjs';
import { readText, fixtureList, fixtureSanctions } from './helpers.mjs';

const secret = new TextEncoder().encode('postern-perps-397-test-edge-secret-00000001').subarray(0, 32);
const NOW = Date.parse('2026-11-17T01:00:00Z');
const A = '0x00000000000000000000000000000000000000a1';
const SAN = '0x983a81ca6fb1e441266d2fbcb7d8e530ac2e05a2';
const clean = () => ({ tor: false, vpnProxy: false, hosting: false });

function rig(over = {}) {
  const seen = [];
  let n = 0;
  const handle = createEdge({
    mode: 'live', list: fixtureList(), sanctions: fixtureSanctions(), secret, origin: 'http://origin.invalid',
    fetchOrigin: async (req) => { seen.push({ url: req.url, method: req.method, headers: Object.fromEntries(req.headers), body: req.body ? await req.text() : null }); return new Response('origin-ok', { status: 200 }); },
    now: () => NOW, randomId: () => (n++).toString(16).padStart(32, '0'), signalsOf: () => clean(), ...over,
  });
  return { handle, seen };
}
const post = (body, headers = {}) => new Request('https://perps.example/v1/actions', { method: 'POST', headers: { 'content-type': 'application/json', 'cf-connecting-ip': '203.0.113.9', ...headers }, body: typeof body === 'string' ? body : JSON.stringify(body) });
const order = (o = {}) => ({ type: 'PlaceOrder', message: { account: A, reduceOnly: false, ...o }, signature: '0x' + '11'.repeat(65) });

test('allowed country: action forwarded once with a MAC the node verifies', async () => {
  const { handle, seen } = rig();
  const res = await handle(post(order()), { country: 'PA' });
  assert.equal(res.status, 200);
  assert.equal(seen.length, 1);
  const f = seen[0];
  assert.equal(f.url, 'http://origin.invalid/v1/actions');
  assert.equal(JSON.parse(f.body).type, 'PlaceOrder', 'body forwarded unchanged');
  const v = verifyAssertion(f.headers[HEADER], f.headers[MAC_HEADER], { secret, nowMs: NOW, replay: new ReplayCache() });
  assert.equal(v.ok, true);
  assert.deepEqual(v.geo, { country: 'PA', subdivision: null, trusted: true });
  assert.equal(v.surface, 'api');
});

test('blocked country: order answered 451 at the edge, nothing forwarded; cancel forwarded (exit-only)', async () => {
  const { handle, seen } = rig();
  const r = await handle(post(order()), { country: 'BR' });
  assert.equal(r.status, 451);
  const b = await r.json();
  assert.equal(b.error.code, 'GEO_RESTRICTED');
  assert.equal(b.error.details.country, 'BR');
  assert.equal(b.error.details.listVersion, '0.2.2');
  assert.equal(seen.length, 0);
  const c = await handle(post({ type: 'CancelAll', message: { account: A } }), { country: 'BR' });
  assert.equal(c.status, 200);
  assert.equal(seen.length, 1);
  const ro = await handle(post(order({ reduceOnly: true })), { country: 'BR' });
  assert.equal(ro.status, 200);
});

test('client-supplied geo headers are stripped and never trusted', async () => {
  const { handle, seen } = rig();
  const forged = { [HEADER]: 'v1;c=PA;s=-;tor=0;vpn=0;host=0;sur=api;ts=1;rid=' + 'f'.repeat(32), [MAC_HEADER]: 'a'.repeat(64), 'x-mock-country': 'PA' };
  const r = await handle(post(order(), forged), { country: 'BR' });
  assert.equal(r.status, 451, 'the mock header is ignored in live mode');
  await handle(new Request('https://perps.example/v1/markets', { headers: forged }), { country: 'BR' });
  const h = seen.at(-1).headers;
  assert.notEqual(h[HEADER], forged[HEADER]);
  assert.match(h[HEADER], /^v1;c=BR;/);
  assert.equal(h['x-mock-country'], undefined);
});

test('Cloudflare T1 and XX are refused for trading; region code becomes the subdivision', async () => {
  const { handle } = rig();
  assert.equal((await handle(post(order()), { country: 'T1' })).status, 451);
  assert.equal((await handle(post(order()), { country: 'XX' })).status, 451);
  assert.equal((await handle(post(order()), {})).status, 451, 'no cf object at all');
  assert.equal((await handle(post(order()), { country: 'UA', regionCode: '43' })).status, 451);
  assert.equal((await handle(post(order()), { country: 'UA', regionCode: '30' })).status, 200);
  assert.equal((await handle(post(order()), { country: 'UA' })).status, 451, 'region unknown');
});

test('sanctioned account or recipient: 451 SANCTIONED, exits included', async () => {
  const { handle, seen } = rig();
  const r = await handle(post(order({ account: SAN })), { country: 'PA' });
  assert.equal(r.status, 451); assert.equal((await r.json()).error.code, 'SANCTIONED');
  const w = await handle(post({ type: 'Withdraw', message: { account: A, recipient: SAN } }), { country: 'BR' });
  assert.equal(w.status, 451); assert.equal((await w.json()).error.code, 'SANCTIONED');
  assert.equal(seen.length, 0);
});

test('VPN and hosting at the edge; hosting on the API passes to the node, which judges the attestation', async () => {
  const vpn = rig({ signalsOf: () => ({ ...clean(), vpnProxy: true }) });
  assert.equal((await vpn.handle(post(order()), { country: 'PA' })).status, 451);
  const host = rig({ signalsOf: () => ({ ...clean(), hosting: true }) });
  assert.equal((await host.handle(post(order()), { country: 'PA' })).status, 200);
  const fwd = host.seen[0].headers;
  const v = verifyAssertion(fwd[HEADER], fwd[MAC_HEADER], { secret, nowMs: NOW });
  assert.equal(v.signals.hosting, true);
  // the node re-decides with the facts and the attestation it holds (none here): refused
  const d = decide(fixtureList(), { mode: 'live', surface: v.surface, kind: 'action', action: { type: 'PlaceOrder', reduceOnly: false }, geo: v.geo, signals: v.signals,
    addresses: [{ role: 'account', address: A }], sanctions: fixtureSanctions(), nowMs: NOW, attestation: null });
  assert.equal(d.code, 'GEO_RESTRICTED');
  assert.deepEqual(d.reasons, ['ANONYMIZER_HOSTING']);
});

test('no signal source in live mode: trading refused, cancel still possible', async () => {
  const { handle } = rig({ signalsOf: undefined });
  assert.equal((await handle(post(order()), { country: 'PA' })).status, 451);
  assert.equal((await handle(post({ type: 'CancelOrder', message: { account: A } }), { country: 'PA' })).status, 200);
});

test('fail closed: no secret, no sanctions list, no geoblock list', async () => {
  const ns = rig({ secret: null });
  const r = await ns.handle(post(order()), { country: 'PA' });
  assert.equal(r.status, 503); assert.equal(ns.seen.length, 0);
  const nl = rig({ sanctions: null });
  assert.equal((await nl.handle(post(order()), { country: 'PA' })).status, 503);
  assert.equal((await nl.handle(post({ type: 'CancelOrder', message: { account: A } }), { country: 'PA' })).status, 200);
  const ng = rig({ list: null });
  assert.equal((await ng.handle(post(order()), { country: 'PA' })).status, 503);
  const g = await ng.handle(new Request('https://perps.example/v1/geo'), { country: 'PA' });
  assert.equal((await g.json()).restricted, true);
  const short = rig({ secret: new Uint8Array(16) });
  assert.equal((await short.handle(post(order()), { country: 'PA' })).status, 503, 'a short secret counts as none');
});

test('malformed and oversized bodies', async () => {
  const { handle, seen } = rig();
  assert.equal((await handle(post('{'), { country: 'PA' })).status, 400);
  assert.equal((await handle(post({ type: 'PlaceOrder' }), { country: 'PA' })).status, 400);
  assert.equal((await handle(post({ type: 'Deposit', message: { account: A } }), { country: 'PA' })).status, 400);
  assert.equal((await handle(post(order({ reduceOnly: 'true' })), { country: 'BR' })).status, 400);
  const big = JSON.stringify(order({ pad: 'x'.repeat(MAX_BODY_BYTES) }));
  assert.equal((await handle(post(big), { country: 'PA' })).status, 413);
  const lying = new Request('https://perps.example/v1/actions', { method: 'POST', headers: { 'content-length': '10' }, body: big });
  assert.equal((await handle(lying, { country: 'PA' })).status, 413, 'actual size counts, not the declared one');
  const bin = new Request('https://perps.example/v1/actions', { method: 'POST', body: new Uint8Array([0xff, 0xfe, 0x00]) });
  assert.equal((await handle(bin, { country: 'PA' })).status, 400, 'invalid UTF-8');
  assert.equal(seen.length, 0);
});

test('/v1/geo, reads and the static app', async () => {
  const { handle, seen } = rig();
  const g = await (await handle(new Request('https://perps.example/v1/geo'), { country: 'BR' })).json();
  assert.equal(g.restricted, true); assert.equal(g.policy, 'exit_only'); assert.equal(g.country, 'BR');
  const read = await handle(new Request('https://perps.example/v1/markets?x=1'), { country: 'BR' });
  assert.equal(read.status, 200, 'public reads are not geoblocked');
  assert.equal(seen.at(-1).url, 'http://origin.invalid/v1/markets?x=1');
  await handle(new Request('https://perps.example/trade/BTC'), { country: 'BR' });
  assert.match(seen.at(-1).headers[HEADER], /sur=ui/);
});

test('dev mode honours the mock country header (for 391 and 393), live mode does not', async () => {
  const dev = rig({ mode: 'dev' });
  assert.equal((await dev.handle(post(order(), { 'x-mock-country': 'BR' }), { country: 'PA' })).status, 451);
  assert.equal((await dev.handle(post(order()), { country: 'BR' })).status, 451);
  assert.equal((await dev.handle(new Request('https://perps.example/v1/geo?country=PA'), {})).status, 200);
});

test('Worker entry point fails closed on broken bindings', async () => {
  const res = await worker.fetch(post(order()), { GEO_MODE: 'live', GEO_LIST_JSON: '{', SANCTIONS_JSON: readText('fixtures/sanctions-fixture.json'), GEO_EDGE_SECRET: 'zz', ORIGIN_URL: 'http://origin.invalid' });
  assert.equal(res.status, 503);
});
