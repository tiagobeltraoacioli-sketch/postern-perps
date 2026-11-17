import test from 'node:test';
import assert from 'node:assert/strict';
import { loadList, ListError } from '../src/list.mjs';
import { readJson, realList, fixtureList } from './helpers.mjs';

function apply(obj, ops) {
  const o = structuredClone(obj);
  for (const op of ops) {
    const path = op.set ?? op.delete;
    let t = o;
    for (const k of path.slice(0, -1)) t = t[k];
    if (op.set) t[path.at(-1)] = op.value; else delete t[path.at(-1)];
  }
  return o;
}

test("392's real list 0.2.1 loads, has no allowed entry, and keeps its guards", () => {
  const l = realList();
  // 0.1.1 -> 0.2.1 (392, merged on main 0cd66a0): every ISO 3166-1 code listed, still no allowed entry; the
  // 0.1.1 guards below are unchanged.
  assert.equal(l.version, '0.2.1');
  assert.ok(Object.keys(l.countries).length >= 249, 'every ISO 3166-1 code is listed');
  assert.match(l.sha256, /^[0-9a-f]{64}$/);
  assert.equal(Object.values(l.countries).filter((e) => e.effect === 'allow').length, 0);
  assert.equal(l.deniedPolicy, 'exit_only');
  assert.deepEqual([...l.exitActions], ['CancelOrder', 'CancelAll', 'PlaceOrder(reduceOnly=true)', 'Withdraw']);
  assert.deepEqual([...l.restrictedSubdivisionCountries], ['UA']);
  for (const cc of ['US', 'CA', 'GB', 'BR', 'CU', 'IR', 'KP', 'SY', 'RU', 'BY', 'CN']) assert.equal(l.countries[cc].effect, 'deny', cc);
  assert.ok(Object.isFrozen(l) && Object.isFrozen(l.countries) && Object.isFrozen(l.countries.BR));
});

test('the fixture list differs from the real one only by its FIXTURE allowed entries and status', () => {
  const real = readJson('../geoblock.json'); const fx = readJson('fixtures/geoblock.fixture.json');
  const { status: _a, countries: rc, ...r } = real; const { status: _b, countries: fc, ...f } = fx;
  assert.deepEqual(f, r);
  assert.deepEqual(Object.keys(fc).sort(), Object.keys(rc).sort(), 'same codes');
  const changed = Object.keys(fc).filter((k) => JSON.stringify(fc[k]) !== JSON.stringify(rc[k]));
  assert.deepEqual(changed.sort(), ['CH', 'PA', 'UA']);
  for (const k of changed) { assert.match(fc[k].name, /FIXTURE/); assert.equal(fc[k].tier, 'allowed'); assert.notEqual(rc[k].tier, 'allowed'); }
  assert.equal(fixtureList().countries.PA.effect, 'allow');
});

test('strict loader vectors', () => {
  const base = readJson('fixtures/geoblock.fixture.json');
  const { cases } = readJson('vectors/list-vectors.json');
  assert.equal(cases.length, 36);
  for (const c of cases) {
    const obj = apply(base, c.ops);
    if (c.accept) {
      assert.doesNotThrow(() => loadList(obj), c.name);
    } else {
      assert.throws(() => loadList(obj), (e) => e instanceof ListError && e.message.includes(c.errorContains), c.name);
    }
  }
});

test('loader refuses non-objects', () => {
  for (const v of [null, [], 'x', 3]) assert.throws(() => loadList(v), ListError);
});
