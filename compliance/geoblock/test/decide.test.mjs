import test from 'node:test';
import assert from 'node:assert/strict';
import { decide, geoStatus } from '../src/decide.mjs';
import { readJson, realList, fixtureList, fixtureSanctions } from './helpers.mjs';

const lists = { fixture: fixtureList(), real: realList() };

test('decision conformance vectors', () => {
  const { cases, sanctionsBuiltMs } = readJson('vectors/decision-vectors.json');
  const sanctions = fixtureSanctions();
  assert.equal(sanctions.builtMs, sanctionsBuiltMs);
  assert.equal(cases.length, 90);   // 88 + 2 on the real list 0.2.2 (R436-7)
  for (const c of cases) {
    const list = c.list === null ? null : lists[c.list];
    const d = decide(list, { ...c.request, sanctions: c.sanctions === null ? null : sanctions });
    const got = { allow: d.allow, code: d.code, httpStatus: d.httpStatus, exitOnly: d.exitOnly };
    if (c.expect.reasons) got.reasons = d.reasons;
    assert.deepEqual(got, c.expect, c.name);
  }
});

test('every refusal of an action is a refusal: allow is false whenever code is not OK*', () => {
  const { cases } = readJson('vectors/decision-vectors.json');
  const sanctions = fixtureSanctions();
  for (const c of cases) {
    const d = decide(c.list === null ? null : lists[c.list], { ...c.request, sanctions: c.sanctions === null ? null : sanctions });
    assert.equal(d.allow, d.code.startsWith('OK'), c.name);
    assert.equal(d.allow, d.httpStatus === 200, c.name);
    if (d.exitOnly) assert.ok(d.restricted, c.name);
  }
});

test('decisions name the list and sanctions versions they were made under', () => {
  const d = decide(lists.fixture, { mode: 'live', surface: 'api', kind: 'action', action: { type: 'CancelOrder' },
    geo: { country: 'BR', trusted: true }, signals: { tor: false, vpnProxy: false, hosting: false },
    addresses: [{ role: 'account', address: '0x00000000000000000000000000000000000000a1' }], sanctions: fixtureSanctions(), nowMs: Date.parse('2026-11-17T01:00:00Z') });
  assert.equal(d.listVersion, '0.2.2');
  assert.match(d.listSha256, /^[0-9a-f]{64}$/);
  assert.match(d.sanctionsVersion, /^2026-11-13\+[0-9a-f]{12}$/);
});

test('exhaustive: no country code opens access under the real list', () => {
  const list = lists.real;
  const L = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
  for (const a of L) for (const b of L) {
    const d = decide(list, { mode: 'live', surface: 'api', kind: 'action', action: { type: 'PlaceOrder', reduceOnly: false },
      geo: { country: a + b, subdivision: null, trusted: true }, signals: { tor: false, vpnProxy: false, hosting: false },
      addresses: [{ role: 'account', address: '0x00000000000000000000000000000000000000a1' }], sanctions: null, nowMs: 0, requireAttestation: false });
    assert.equal(d.allow, false, a + b);
  }
});

test('exhaustive over the fixture: only PA, CH and UA (with a known unrestricted region) open a non-exit action', () => {
  const list = lists.fixture;
  const sanctions = fixtureSanctions();
  const open = [];
  const L = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ';
  for (const a of L) for (const b of L) for (const sub of [null, `${a}${b}-30`, `${a}${b}-43`]) {
    for (const type of ['PlaceOrder', 'UpdateLeverage', 'UpdateMargin', 'ApproveAgent']) {
      const d = decide(list, { mode: 'live', surface: 'api', kind: 'action', action: { type, reduceOnly: false },
        geo: { country: a + b, subdivision: sub, trusted: true }, signals: { tor: false, vpnProxy: false, hosting: false },
        addresses: [{ role: 'account', address: '0x00000000000000000000000000000000000000a1' }], sanctions,
        nowMs: Date.parse('2026-11-17T01:00:00Z'), attestation: { status: 'valid', residenceCountry: 'PA' } });
      if (d.allow) open.push(`${a}${b}${sub ? '/' + sub : ''}:${type}`);
    }
  }
  const countries = [...new Set(open.map((x) => x.split(':')[0]))].sort();
  assert.deepEqual(countries, ['CH', 'CH/CH-30', 'CH/CH-43', 'PA', 'PA/PA-30', 'PA/PA-43', 'UA/UA-30']);
});

test('geoStatus matches docs/API.md section 6 shape', () => {
  const g = geoStatus(lists.fixture, { mode: 'live', surface: 'api', geo: { country: 'BR', trusted: true }, signals: { tor: false, vpnProxy: false, hosting: false } });
  assert.equal(g.country, 'BR'); assert.equal(g.restricted, true); assert.equal(g.policy, 'exit_only');
  assert.equal(g.listVersion, '0.2.2'); assert.equal(g.reasonCode, 'JURISDICTION'); assert.equal(g.sanctioned, false);
  assert.match(g.message, /withdraw/);
  const o = geoStatus(lists.fixture, { mode: 'live', surface: 'api', geo: { country: 'PA', trusted: true }, signals: { tor: false, vpnProxy: false, hosting: false } });
  assert.equal(o.restricted, false); assert.equal(o.policy, 'open'); assert.equal(o.message, null);
  const n = geoStatus(null, {});
  assert.equal(n.restricted, true); assert.equal(n.policy, 'actions_refused');
});

test('decide never throws on hostile input', () => {
  const junk = [undefined, null, {}, { mode: 'live' }, { mode: 'live', kind: 'action', surface: 'api', action: null },
    { mode: 'live', kind: 'action', surface: 'api', action: { type: 'PlaceOrder', reduceOnly: true }, addresses: 'x' },
    { mode: 'live', kind: 'action', surface: 'api', action: { type: '__proto__' }, addresses: [{ role: 'account', address: {} }] },
    { mode: 'live', kind: 'action', surface: 'api', action: { type: 'CancelOrder' }, addresses: [null] },
    { mode: 'live', kind: 'action', surface: 'api', action: { type: 'CancelOrder' }, addresses: [{ role: 'account', address: '0x00000000000000000000000000000000000000a1' }], geo: { country: 7, subdivision: {} }, signals: 'x' }];
  for (const j of junk.slice(0, -1)) {
    const d = decide(lists.fixture, j);
    assert.equal(d.allow, false);
  }
  // junk geography and signals = unknown location: a cancel still passes as an exit, nothing else
  const last = decide(lists.fixture, junk.at(-1));
  assert.equal(last.code, 'OK_EXIT_ONLY');
  assert.equal(decide(lists.fixture, { ...junk.at(-1), action: { type: 'PlaceOrder', reduceOnly: false } }).allow, false);
});

// R436-7 (list 0.2.2). Until 0.2.2 the vector "blocked country BR, revoke agent refused" pinned that an action the
// list does not name as an exit action is refused for a denied country. The list now names RevokeAgent, so that
// vector expects the exit; the list-driven refusal it stood for is pinned here against a list WITHOUT the entry.
test('an action the list does not name as an exit action is refused for a denied country (list-driven, both ways)', async () => {
  const { loadList } = await import('../src/list.mjs');
  const json = readJson('fixtures/geoblock.fixture.json');
  const req = (type) => ({ mode: 'live', surface: 'api', kind: 'action', action: { type }, geo: { country: 'BR', subdivision: null, trusted: true },
    signals: { tor: false, vpnProxy: false, hosting: false }, addresses: [{ role: 'account', address: '0x00000000000000000000000000000000000000a1' }],
    sanctions: fixtureSanctions(), nowMs: 1794877200000 });
  const without = loadList({ ...json, denied_policy: { ...json.denied_policy, exit_only_actions: json.denied_policy.exit_only_actions.filter((a) => a !== 'RevokeAgent') } });
  const d0 = decide(without, req('RevokeAgent'));
  assert.deepEqual([d0.allow, d0.code, d0.httpStatus, d0.exitOnly], [false, 'GEO_RESTRICTED', 451, false]);
  const d1 = decide(lists.fixture, req('RevokeAgent'));
  assert.deepEqual([d1.allow, d1.code, d1.httpStatus, d1.exitOnly], [true, 'OK_EXIT_ONLY', 200, true]);
  // opening authority stays refused whatever the list says about revoking it
  for (const l of [without, lists.fixture, lists.real]) assert.equal(decide(l, req('ApproveAgent')).code, 'GEO_RESTRICTED');
  // and the loader still refuses a list that tries to name an exposure-opening action as an exit
  assert.throws(() => loadList({ ...json, denied_policy: { ...json.denied_policy, exit_only_actions: [...json.denied_policy.exit_only_actions, 'ApproveAgent'] } }), /not an exit action/);
});
