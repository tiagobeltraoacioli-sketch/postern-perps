import test from 'node:test';
import assert from 'node:assert/strict';
import { encodeAssertion, macAssertion, verifyAssertion, ReplayCache, MAX_AGE_MS, MAX_FUTURE_MS } from '../src/geo-assertion.mjs';
import { readJson } from './helpers.mjs';

// Throwaway test secret: 32 bytes derived from a public label. Never a real secret.
const secret = new TextEncoder().encode('postern-perps-397-test-geo-secret-0001').subarray(0, 32);
const T = 1794878412300;
const base = { country: 'BR', subdivision: 'BR-SP', signals: { tor: false, vpnProxy: true, hosting: null }, surface: 'api', tsMs: T, requestId: '0123456789abcdef0123456789abcdef' };

test('round trip', () => {
  const h = encodeAssertion(base);
  assert.equal(h, 'v1;c=BR;s=BR-SP;tor=0;vpn=1;host=-;sur=api;ts=1794878412300;rid=0123456789abcdef0123456789abcdef');
  const r = verifyAssertion(h, macAssertion(secret, h), { secret, nowMs: T + 1000, replay: new ReplayCache() });
  assert.equal(r.ok, true);
  assert.deepEqual(r.geo, { country: 'BR', subdivision: 'BR-SP', trusted: true });
  assert.deepEqual(r.signals, { tor: false, vpnProxy: true, hosting: null });
});

test('conformance vectors (shared with the Rust port)', () => {
  const { cases } = readJson('vectors/geo-assertion-vectors.json');
  const sec = new TextEncoder().encode(readJson('vectors/geo-assertion-vectors.json').secretUtf8);
  for (const c of cases) {
    const r = verifyAssertion(c.header, c.mac ?? macAssertion(sec, c.header), { secret: sec, nowMs: c.nowMs, replay: null });
    assert.equal(r.ok ? 'ok' : r.reason, c.expect, c.name);
  }
});

test('any change to the header breaks the MAC', () => {
  const h = encodeAssertion(base); const mac = macAssertion(secret, h);
  for (const t of [h.replace('c=BR', 'c=PA'), h.replace('vpn=1', 'vpn=0'), h.replace('sur=api', 'sur=ui'), h + ';'] ) {
    assert.equal(verifyAssertion(t, mac, { secret, nowMs: T }).reason, 'mac');
  }
  const other = new Uint8Array(32).fill(7);
  assert.equal(verifyAssertion(h, macAssertion(other, h), { secret, nowMs: T }).reason, 'mac');
});

test('freshness window and replay', () => {
  const h = encodeAssertion(base); const mac = macAssertion(secret, h);
  assert.equal(verifyAssertion(h, mac, { secret, nowMs: T + MAX_AGE_MS }).ok, true);
  assert.equal(verifyAssertion(h, mac, { secret, nowMs: T + MAX_AGE_MS + 1 }).reason, 'stale');
  assert.equal(verifyAssertion(h, mac, { secret, nowMs: T - MAX_FUTURE_MS - 1 }).reason, 'future');
  const replay = new ReplayCache();
  assert.equal(verifyAssertion(h, mac, { secret, nowMs: T, replay }).ok, true);
  assert.equal(verifyAssertion(h, mac, { secret, nowMs: T + 10, replay }).reason, 'replay');
  const full = new ReplayCache({ maxEntries: 1 });
  assert.equal(full.add('a', T, T), true);
  assert.equal(full.add('b', T, T), false, 'full cache refuses rather than forgets');
  assert.equal(full.add('c', T + 60_000, T + 60_000), true, 'expired ids are evicted');
});

test('short secrets and bad fields are refused at encode/MAC time', () => {
  assert.throws(() => macAssertion(new Uint8Array(31), 'x'), /at least 32/);
  assert.throws(() => encodeAssertion({ ...base, country: 'Brazil' }), /country/);
  assert.throws(() => encodeAssertion({ ...base, requestId: 'x' }), /requestId/);
  assert.throws(() => encodeAssertion({ ...base, surface: 'cli' }), /surface/);
});
