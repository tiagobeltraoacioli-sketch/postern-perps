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

test("392's real list 0.1.1 loads, has no allowed entry, and keeps its guards", () => {
  const l = realList();
  assert.equal(l.version, '0.1.1');
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
  for (const [k, v] of Object.entries(rc)) assert.deepEqual(fc[k], v, k);
  const added = Object.keys(fc).filter((k) => !(k in rc));
  assert.deepEqual(added.sort(), ['CH', 'PA', 'UA']);
  for (const k of added) assert.match(fc[k].name, /FIXTURE/);
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
