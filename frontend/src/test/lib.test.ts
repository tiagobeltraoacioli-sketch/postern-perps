import { describe, expect, it, beforeEach } from 'vitest';
import { PX, SZ, USDC, mulDiv, parseFixed, toDecimalString, toTick } from '../lib/fixed';
import { formatCompactUsd, formatCountdown, formatFixed, formatRate, MINUS } from '../lib/format';
import { fee, fundingPayment, initialMargin, liquidationPrice, maintenanceMargin, maxLeverageAt, pnl, sizeForBudget, notional, accountSummary, type Tier } from '../lib/calc';
import { nextNonce, NonceError, resetNonces } from '../lib/nonce';
import { signingProblem, tableProblems } from '../lib/pins';
import { agentFor, agentSigner, createAgent, forgetAgent, keepAgent, resetAgents } from '../lib/agent';
import { market, markets, NOW } from './fixtures';

describe('fixed point', () => {
  it('parses exactly and refuses extra places', () => {
    expect(parseFixed('12.5', 6)).toBe(12_500_000n);
    expect(parseFixed('-0.001', 6)).toBe(-1000n);
    expect(parseFixed('1.0000001', 6)).toBeNull();
    expect(parseFixed('.', 6)).toBeNull();
    expect(toDecimalString(-1_500_000n, 6)).toBe('-1.5');
  });
  it('rounds the true quotient as named', () => {
    expect(mulDiv(-7n, 1n, 2n, 'floor')).toBe(-4n);
    expect(mulDiv(-7n, 1n, 2n, 'ceil')).toBe(-3n);
    expect(mulDiv(7n, 1n, 2n, 'half')).toBe(4n);
    expect(toTick(1_234n, 5n, 'floor')).toBe(1_230n);
    expect(toTick(1_234n, 5n, 'ceil')).toBe(1_235n);
  });
  it('formats with a true minus, grouping and stated rounding', () => {
    expect(formatFixed(-1_234_567_890n, 6, { dp: 2 })).toBe(`${MINUS}1,234.57`);
    expect(formatFixed(1_234_565_000n, 6, { dp: 2, rounding: 'floor' })).toBe('1,234.56');
    expect(formatRate(12_500n)).toBe('+0.0013%'); // 0.00125% in the engine's 1e-9 (round 1: 1,250 in 1e-8)
    expect(formatCompactUsd(18_233_412_550_000n)).toBe('18.23M');
    expect(formatCountdown(3_723_000)).toBe('1:02:03');
  });
});

// Round 2: calc.ts now runs on the engine's arithmetic (lib/exact.ts, pinned by the engine's 755 worked-example
// values in exact.test.ts). The six round-1 examples below are KEPT with the same numbers and the same properties,
// restated in the new signatures (a position is given as size and price, not as a notional already cut to USDC;
// tiers are the published floors and fractions, with no precomputed deduction; the funding rate is in the engine's
// 1e-9). Where round 1's own formula differed from the engine, a new case shows the difference.
describe('calc worked examples (round-1 fixture, restated on the engine\'s arithmetic)', () => {
  const tiers: Tier[] = [
    { floor: 0n, imfPpm: 20_000n, mmfPpm: 10_000n, maxLeverage: 50 },
    { floor: 1_000_000n * USDC, imfPpm: 40_000n, mmfPpm: 20_000n, maxLeverage: 25 },
    { floor: 5_000_000n * USDC, imfPpm: 100_000n, mmfPpm: 50_000n, maxLeverage: 10 },
  ];
  it('notional, PnL, margin and fee of 0.5 BTC long from 66,800 at mark 67,000', () => {
    const size = SZ / 2n;
    expect(notional(size, 67_000n * PX)).toBe(33_500n * USDC);
    expect(pnl(size, 66_800n * PX, 67_000n * PX)).toBe(100n * USDC);
    expect(maintenanceMargin(tiers, size, 67_000n * PX)).toBe(335n * USDC);
    expect(initialMargin(tiers, size, 67_000n * PX, 10)).toBe(3_350n * USDC);
    expect(fee(size, 67_000n * PX, 450n)).toBe(15_075_000n);
  });
  it('the requirement is continuous across tier floors (was: tier deductions keep it continuous)', () => {
    // 1,000,000 at 1% = 10,000; 2,000,000 = 10,000 + 1,000,000 at 2% = 30,000: the numbers of round 1.
    expect(maintenanceMargin(tiers, 10n * SZ, 100_000n * PX)).toBe(10_000n * USDC);
    expect(maintenanceMargin(tiers, 20n * SZ, 100_000n * PX)).toBe(30_000n * USDC);
    // One size unit across the floor moves the requirement by that unit's share only: no jump.
    const below = maintenanceMargin(tiers, 10n * SZ - 1n, 100_000n * PX);
    const above = maintenanceMargin(tiers, 10n * SZ + 1n, 100_000n * PX);
    // (one size unit at 100,000 is 1,000 USDC units of notional: 10 units at 1% below the floor, 20 at 2% above)
    expect(10_000n * USDC - below).toBe(10n);
    expect(above - 10_000n * USDC).toBe(20n);
    expect(maxLeverageAt(tiers, 1_000_000n * USDC)).toBe(50);
    expect(maxLeverageAt(tiers, 1_000_000n * USDC + 1n)).toBe(25);
  });
  it('isolated liquidation price: 0.5 BTC long from 66,800 with 1,000 USDC, rounded up to the tick', () => {
    // (33,400 - 1,000) / (0.5 x 0.99) = 65,454.54...; the tick is 0.5, a long rounds up: 65,455.0.
    expect(liquidationPrice({ size: SZ / 2n, entryPx: 66_800n * PX, backing: 1_000n * USDC, tiers, tick: PX / 2n })).toBe(65_455n * PX);
    // A short rounds down: (33,400 + 1,000) / (0.5 x 1.01) = 68,118.81...: 68,118.5.
    expect(liquidationPrice({ size: -SZ / 2n, entryPx: 66_800n * PX, backing: 1_000n * USDC, tiers, tick: PX / 2n })).toBe(68_118_50_000_000n);
    // More margin than the position can lose: no price liquidates a long.
    expect(liquidationPrice({ size: SZ / 2n, entryPx: 66_800n * PX, backing: 40_000n * USDC, tiers, tick: PX / 2n })).toBeNull();
  });
  it('funding: a long pays at a positive rate; the payment is rounded against the payer', () => {
    // 0.00125% per hour is 12,500 in the engine's 1e-9 (round 1 wrote it 1,250 in 1e-8): the same payments.
    expect(fundingPayment(SZ / 2n, 67_000n * PX, 12_500n)).toBe(-418_750n);
    expect(fundingPayment(-SZ / 2n, 67_000n * PX, 12_500n)).toBe(418_750n);
    expect(fundingPayment(SZ / 3n, 67_000n * PX, 12_500n)).toBe(-279_167n);
    // What round 1's 1e-8 could not hold: a rate with a ninth digit.
    expect(fundingPayment(SZ / 2n, 67_000n * PX, 12_345n)).toBe(-413_558n);
  });
  it('size for a budget never exceeds it', () => {
    const size = sizeForBudget({ budget: 1_000n * USDC, px: 50_000n * PX, leverage: 10, feePpm: 450n, step: 10_000n, tiers });
    expect(size).toBe(19_910_000n);
    expect(initialMargin(tiers, size, 50_000n * PX, 10) + fee(size, 50_000n * PX, 450n)).toBeLessThanOrEqual(1_000n * USDC);
    expect(initialMargin(tiers, size + 10_000n, 50_000n * PX, 10) + fee(size + 10_000n, 50_000n * PX, 450n)).toBeGreaterThan(1_000n * USDC);
    // A tier that asks for more than the leverage fraction lowers the answer (round 1 ignored the tier's imf).
    const steep: Tier[] = [{ floor: 0n, imfPpm: 200_000n, mmfPpm: 100_000n, maxLeverage: 5 }];
    const capped = sizeForBudget({ budget: 1_000n * USDC, px: 50_000n * PX, leverage: 10, feePpm: 450n, step: 10_000n, tiers: steep });
    expect(capped).toBe(9_970_000n);
    expect(initialMargin(steep, capped, 50_000n * PX, 10) + fee(capped, 50_000n * PX, 450n)).toBeLessThanOrEqual(1_000n * USDC);
  });
  it('account summary: cross equity, isolated margin set aside, margin ratio rounded up', () => {
    const s = accountSummary(25_000n * USDC, [
      { market: 'BTC-USD', size: SZ / 2n, entryPx: 66_800n * PX, markPx: 67_000n * PX, leverage: 10, isolated: false, isolatedMargin: 0n, tiers, tick: PX / 2n },
    ]);
    expect(s.equity).toBe(25_100n * USDC);
    expect(s.available).toBe(25_100n * USDC - 3_350n * USDC);
    expect(s.marginRatioPpm).toBe(mulDiv(335n * USDC, 1_000_000n, 25_100n * USDC, 'ceil'));
    const withIsolated = accountSummary(25_000n * USDC, [
      { market: 'BTC-USD', size: SZ / 2n, entryPx: 66_800n * PX, markPx: 67_000n * PX, leverage: 10, isolated: false, isolatedMargin: 0n, tiers, tick: PX / 2n },
      { market: 'ETH-USD', size: -SZ, entryPx: 3_500n * PX, markPx: 3_400n * PX, leverage: 5, isolated: true, isolatedMargin: 700n * USDC, tiers, tick: PX / 100n },
    ]);
    // SPEC 6.1: an isolated position's margin is NOT part of the balance. The cross side is untouched by it; the
    // account's equity adds the isolated margin and that position's result (short 1 ETH from 3,500 at 3,400: +100).
    expect(withIsolated.crossEquity).toBe(25_100n * USDC);
    expect(withIsolated.available).toBe(25_100n * USDC - 3_350n * USDC);
    expect(withIsolated.marginUsed).toBe(3_350n * USDC + 700n * USDC);
    expect(withIsolated.equity).toBe(25_000n * USDC + 700n * USDC + 100n * USDC + 100n * USDC);
  });
  it('where round 1 differed from the engine: the leverage fraction rounds UP and the fee is on the exact notional', () => {
    // Leverage 3 is 333,334 ppm (R395-15), not notional / 3: 100.000001 USDC of notional needs 33.333401, not 33.333334.
    const one: Tier[] = [{ floor: 0n, imfPpm: 10_000n, mmfPpm: 5_000n, maxLeverage: 100 }];
    expect(initialMargin(one, 100_000_001n, 100n * PX, 3)).toBe(33_333_401n);
    // A fee on a notional with a fraction of a USDC unit: 0.00000001 BTC at 67,250.5 = 0.000672505 USD; the engine
    // charges ceil on the exact value (1 unit), where flooring the notional to USDC first would have charged on 672.
    expect(fee(1n, 67_250_50_000_000n, 450n)).toBe(1n);
    expect(fee(3n * SZ, 67_251_30_000_000n, 450n)).toBe(90_789_255n);
  });
});

describe('nonce', () => {
  beforeEach(() => resetNonces());
  it('is the clock, or last + 1 within one millisecond', () => {
    expect(nextNonce('0xA', NOW)).toBe(BigInt(NOW));
    expect(nextNonce('0xa', NOW)).toBe(BigInt(NOW) + 1n);
    expect(nextNonce('0xb', NOW)).toBe(BigInt(NOW));
  });
  it('follows a published nonce only inside the SPEC 2.5 future bound (30 s), refuses beyond it', () => {
    expect(nextNonce('0xc', NOW, BigInt(NOW) + 5_000n)).toBe(BigInt(NOW) + 5_000n);
    expect(() => nextNonce('0xd', NOW, BigInt(NOW) + 30_001n)).toThrow(NonceError);
  });
  it('R395-11: a far-ahead published nonce is never stored, so the next signature is back on the clock', () => {
    expect(() => nextNonce('0xe', NOW, BigInt(NOW) + 86_400_000n)).toThrow(NonceError);
    expect(nextNonce('0xe', NOW + 1)).toBe(BigInt(NOW) + 1n);
    expect(JSON.parse(localStorage.getItem('postern.perps.nonce.v1')!)['0xe']).toBe(String(NOW + 1));
  });
  it('refuses when the clock moved back beyond the window instead of signing a nonce the engine rejects', () => {
    nextNonce('0xf', NOW);
    expect(() => nextNonce('0xf', NOW - 60_000)).toThrow(/moved back/);
    expect(nextNonce('0xf', NOW - 10_000)).toBe(BigInt(NOW) + 1n);
  });
});

describe('market pins', () => {
  it('the published fixture equals the pin', () => {
    for (const m of markets()) expect(signingProblem(m)).toBeNull();
    expect(tableProblems(markets()).size).toBe(0);
  });
  it('any signing field differing is a problem; a market the pin does not know is a problem', () => {
    const btc = market('BTC-USD');
    expect(signingProblem({ ...btc, marketId: 2 })).toMatch(/market id 2/);
    expect(signingProblem({ ...btc, tick: btc.tick * 2n })).toMatch(/tick/);
    expect(signingProblem({ ...btc, step: btc.step * 10n })).toMatch(/step/);
    expect(signingProblem({ ...btc, base: 'XBT' })).toMatch(/base/);
    expect(signingProblem({ ...btc, symbol: 'DOGE-USD' })).toMatch(/not in this interface/);
  });
  it('two markets claiming one id are both refused', () => {
    const ms = markets().map((m) => (m.symbol === 'SOL-USD' ? { ...m, marketId: 1 } : m));
    const p = tableProblems(ms);
    expect(p.get('BTC-USD')).toMatch(/both claim/);
    expect(p.get('SOL-USD')).toMatch(/both claim|market id 1/);
  });
});

describe('agent keys', () => {
  beforeEach(() => resetAgents());
  it('generated here, kept in memory by default, persisted only when asked, forgotten when expired', () => {
    const owner = '0x1111111111111111111111111111111111111111';
    const r = createAgent(owner, 'bot', NOW + 3_600_000);
    expect(r.agent).toMatch(/^0x[0-9a-f]{40}$/);
    keepAgent(r, false);
    expect(localStorage.getItem('postern.perps.agents.v2')).toBe('[]');
    expect(agentFor(owner, NOW)?.agent).toBe(r.agent);
    keepAgent(r, true);
    expect(JSON.parse(localStorage.getItem('postern.perps.agents.v2')!)).toHaveLength(1);
    expect(agentFor(owner, NOW + 3_600_001)).toBeNull();
    expect(localStorage.getItem('postern.perps.agents.v2')).toBe('[]');
    forgetAgent(owner);
  });
  it('an agent signer is marked as an agent with its expiry', () => {
    const r = createAgent('0x1111111111111111111111111111111111111111', 'bot', 123);
    const s = agentSigner(r);
    expect(s.kind).toBe('agent');
    expect(s.expiresAt).toBe(123);
    expect(s.address).toBe(r.agent);
  });
});
