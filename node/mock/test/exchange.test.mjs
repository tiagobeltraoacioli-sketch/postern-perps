// The mock exchange: determinism, exact conservation, SPEC worked example, flows. UNAUDITED prototype.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { Exchange, mockKey, FEE_ACCOUNT } from '../lib/exchange.mjs';
import { loadFixtures, startMock } from '../server.mjs';
import { admit, geoDecision, toTyped } from '../lib/admission.mjs';
import { fromCompliance } from '../lib/markets.mjs';
import { sign, addressOf } from '../lib/secp256k1.mjs';
import { typedDataHash } from '../lib/eip712.mjs';
import { hex } from '../lib/keccak.mjs';

const fx = loadFixtures();
const make = (seed = 7) => new Exchange({ seed, marketsFx: fx.markets, eip712Fx: fx.eip712 });
const ctx = (x) => ({ geo: geoDecision(fx.geo, fx.mockGeo, 'ZZ'), sanctioned: new Set(), nonceWindowMs: [120_000, 30_000], now: x.time });
function act(x, key, type, msg) {
  const typed = toTyped(x, type, msg, true);
  const signature = hex(sign(typedDataHash(x.eip712.domain, type, x.eip712.types[type], typed), key));
  const a = admit(x, { type, message: msg, signature, wire: true }, ctx(x));
  x.pending.push(a);
  return a;
}
const run = (x, n) => {
  for (let i = 0; i < n; i++) x.produceBlock();
};

test('same seed and inputs give the same log and state root; another seed does not', () => {
  const a = make(7), b = make(7), c = make(8);
  run(a, 60);
  run(b, 60);
  run(c, 60);
  assert.equal(a.lastLogHash, b.lastLogHash);
  assert.equal(a.blocks.get(a.height).stateRoot, b.blocks.get(b.height).stateRoot);
  assert.notEqual(a.lastLogHash, c.lastLogHash);
});

test('conservation holds exactly and sizes sum to zero through trading, funding, liquidation and withdrawals', () => {
  const x = make(11);
  const key = mockKey('test/trader'), addr = addressOf(key);
  x.seedDemo(addr);
  const btc = x.byId.get(1);
  let nonce = BigInt(x.time);
  const n = () => String(nonce++);
  const px = (p) => String(p * 10n ** BigInt(8 - btc.pd));
  const sz = (s) => String(s * 10n ** BigInt(8 - btc.sd));
  for (let i = 0; i < 30; i++) {
    const isBuy = i % 2 === 0;
    const price = isBuy ? btc.mark + btc.mark / 100n : btc.mark - btc.mark / 100n;
    act(x, key, 'PlaceOrder', { account: addr, nonce: n(), marketId: 1, isBuy, orderType: 1, timeInForce: 1, price: px(x.roundTick(btc, price)), size: sz(btc.minSize * 3n), reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) });
    run(x, 3);
    nonce = nonce > BigInt(x.time) ? nonce : BigInt(x.time);
    const c = x.conservation();
    assert.ok(c.ok, `conservation at block ${x.height}: ${c.lhs} vs ${c.rhs}`);
    assert.ok(c.sizesZero, 'sizes sum to zero');
  }
  // Cross a funding boundary (hourly) and liquidate.
  x.time = Number(btc.nextFunding) - x.blockMs;
  run(x, 2);
  assert.ok(x.conservation().ok, 'after funding');
  const ev = x.newEvents();
  x.liquidate(x.account(addr), null, ev);
  assert.ok(x.conservation().ok, 'after liquidation');
  assert.ok([...x.account(addr).positions.values()].every((p) => p.size === 0n));
  act(x, key, 'Withdraw', { account: addr, nonce: String(BigInt(x.time) + 1n), amount: '50000000', recipient: addr });
  run(x, 30);
  const c = x.conservation();
  assert.ok(c.ok && c.sizesZero, `final ${c.lhs} vs ${c.rhs}`);
  const w = x.account(addr).withdrawals.find((w) => !w.demo);
  assert.equal(w.status, 'paid', 'withdrawal reaches paid through finalized, certified, submitted');
  assert.equal(w.amount, 49_000_000n, 'gross 50 USDC minus the 1 USDC fee (SPEC S-9.2, risk.json)');
});

test('SPEC section 15 worked example: 0.5 BTC at 67,250.0 pays 15.13125 USDC taker fee; IR 3,362.5; MR at tier 1', () => {
  const x = make(3);
  const btc = x.byId.get(1);
  const n = x.notional(btc, 5000n, 672500n);
  assert.equal(n, 33_625_000_000n);
  const fee = (n * btc.takerBps + 10_000n * 100n - 1n) / (10_000n * 100n);
  assert.equal(fee, 15_131_250n);
  // markets.json 0.2.0 tier 1 of BTC: IMF 50000 ppm, MMF 25000 ppm; leverage 10 -> imf' = 100000.
  assert.equal((n * 100000n) / 1_000_000n, 3_362_500_000n);
  assert.equal((n * 25000n) / 1_000_000n, 840_625_000n);
});

test('wire prices are SPEC fixed 1e-8 integers: finer than the tick is TICK, not rounded', () => {
  const x = make(5);
  const key = mockKey('test/tick'), addr = addressOf(key);
  x.seedDemo(addr);
  const btc = x.byId.get(1);
  const base = { account: addr, marketId: 1, isBuy: true, orderType: 0, timeInForce: 0, size: String(btc.minSize * 10n ** BigInt(8 - btc.sd)), reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) };
  const good = (btc.mark / 2n) * 10n ** BigInt(8 - btc.pd);
  act(x, key, 'PlaceOrder', { ...base, nonce: String(x.time), price: String(good + 1n) });
  act(x, key, 'PlaceOrder', { ...base, nonce: String(x.time + 1), price: String(good - (good % (btc.tick * 10n ** 7n))) });
  const ev = x.produceBlock();
  const results = [...ev.accounts.get(addr).actions].map((a) => a.result);
  assert.equal(results[0].error, 'TICK');
  assert.equal(results[1].ok, true);
});

test('human-unit requests convert to the same typed message as wire requests (one signature, one meaning)', () => {
  const x = make(5);
  const addr = addressOf(mockKey('test/human'));
  const human = toTyped(x, 'PlaceOrder', { account: addr, nonce: '5', marketId: 1, side: 'buy', orderType: 'limit', timeInForce: 'GTC', price: '67250.5', size: '0.25', reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) }, false);
  assert.equal(human.price, '6725050000000');
  assert.equal(human.size, '25000000');
  assert.equal(human.isBuy, true);
  assert.throws(() => toTyped(x, 'PlaceOrder', { account: addr, nonce: '5', marketId: 1, side: 'buy', orderType: 'limit', timeInForce: 'GTC', price: '67250.123456789', size: '0.25', reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) }, false), /decimals/);
  assert.throws(() => toTyped(x, 'PlaceOrder', { account: addr, nonce: '5', marketId: 1, side: 'up', orderType: 'limit', timeInForce: 'GTC', price: '1', size: '1', reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) }, false), /side/);
});

test('geoblocking: deny by default; the mock allow-list never overrides a listed entry', () => {
  for (const c of ['BR', 'US', 'GB', 'CA']) assert.equal(geoDecision(fx.geo, fx.mockGeo, c).allowed, false, c);
  assert.equal(geoDecision(fx.geo, fx.mockGeo, null).allowed, false);
  assert.equal(geoDecision(fx.geo, fx.mockGeo, 'QQ').allowed, false);
  assert.equal(geoDecision(fx.geo, fx.mockGeo, 'ZZ').allowed, true);
  assert.equal(geoDecision(fx.geo, { ...fx.mockGeo, mock_allowed_countries: ['BR'] }, 'BR').allowed, false);
  assert.equal(geoDecision(fx.geo, fx.mockGeo, 'BR').policy, 'exit_only');
});

test('the mock refuses to start when its allow-list names a listed country', async () => {
  const bad = { ...fx.mockGeo, mock_allowed_countries: ['US'] };
  const { writeFileSync, mkdtempSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const dir = mkdtempSync(`${tmpdir()}/a393-`);
  writeFileSync(`${dir}/g.json`, JSON.stringify(bad));
  await assert.rejects(startMock({ port: 0, static: true, mockGeoFile: `${dir}/g.json` }), /lists as blocked/);
});

test('markets converter refuses what a SPEC genesis converter refuses', () => {
  const mk = JSON.parse(readFileSync(new URL('../../../compliance/markets.json', import.meta.url)));
  const risk = JSON.parse(readFileSync(new URL('../../../compliance/risk.json', import.meta.url)));
  const sim = JSON.parse(readFileSync(new URL('../fixtures/sim.json', import.meta.url)));
  assert.doesNotThrow(() => fromCompliance(mk, risk, sim));
  const clone = () => JSON.parse(JSON.stringify(mk));
  let m = clone();
  m.markets[0].priceDecimals = 2;
  assert.throws(() => fromCompliance(m, risk, sim), /priceDecimals/);
  m = clone();
  m.markets[0].tiers[1].floor = '0';
  assert.throws(() => fromCompliance(m, risk, sim), /ascending/);
  m = clone();
  m.markets[0].tiers[0].mmfPpm = 4000;
  assert.throws(() => fromCompliance(m, risk, sim), /markBandBps/);
  m = clone();
  m.markets[0].tiers[0].imfPpm = m.markets[0].tiers[0].mmfPpm;
  assert.throws(() => fromCompliance(m, risk, sim), /IMF/);
});

test('protocol fees accrue to the PROTOCOL_FEE system account (SPEC 8), never to a user', () => {
  const x = make(9);
  run(x, 5);
  const before = x.account(FEE_ACCOUNT).collateral;
  const key = mockKey('test/fees'), addr = addressOf(key);
  x.seedDemo(addr);
  const btc = x.byId.get(1);
  act(x, key, 'PlaceOrder', { account: addr, nonce: String(x.time), marketId: 1, isBuy: true, orderType: 1, timeInForce: 1, price: String(x.roundTick(btc, btc.mark * 101n / 100n) * 10n ** 7n), size: String(btc.minSize * 10n ** 4n), reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) });
  run(x, 1);
  assert.ok(x.account(FEE_ACCOUNT).collateral > before);
  assert.equal(FEE_ACCOUNT, '0x0000000000000000000000000000000000000001');
});
