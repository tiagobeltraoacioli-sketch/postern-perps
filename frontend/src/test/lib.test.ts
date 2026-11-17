import { describe, expect, it, beforeEach } from 'vitest';
import { PX, SZ, USDC, mulDiv, parseFixed, toDecimalString, toTick } from '../lib/fixed';
import { formatCompactUsd, formatCountdown, formatFixed, formatRate, MINUS } from '../lib/format';
import { fee, fundingPayment, initialMargin, liquidationPrice, maintenanceMargin, pnl, sizeForBudget, withDeductions, notional, accountSummary } from '../lib/calc';
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
    expect(formatRate(1_250n)).toBe('+0.0013%');
    expect(formatCompactUsd(18_233_412_550_000n)).toBe('18.23M');
    expect(formatCountdown(3_723_000)).toBe('1:02:03');
  });
});

describe('calc worked examples (frontend fixture until SPEC.md publishes its own)', () => {
  const tiers = withDeductions([
    { upTo: 1_000_000n * USDC, maxLeverage: 50, mmrPpm: 10_000n },
    { upTo: 5_000_000n * USDC, maxLeverage: 25, mmrPpm: 20_000n },
    { upTo: null, maxLeverage: 10, mmrPpm: 50_000n },
  ]);
  it('notional, PnL, margin and fee of 0.5 BTC long from 66,800 at mark 67,000', () => {
    const size = SZ / 2n;
    expect(notional(size, 67_000n * PX)).toBe(33_500n * USDC);
    expect(pnl(size, 66_800n * PX, 67_000n * PX)).toBe(100n * USDC);
    expect(maintenanceMargin(tiers, 33_500n * USDC)).toBe(335n * USDC);
    expect(initialMargin(33_500n * USDC, 10)).toBe(3_350n * USDC);
    expect(fee(33_500n * USDC, 450n)).toBe(15_075_000n);
  });
  it('tier deductions keep the requirement continuous', () => {
    expect(tiers[1].deduction).toBe(10_000n * USDC);
    expect(maintenanceMargin(tiers, 1_000_000n * USDC)).toBe(10_000n * USDC);
    expect(maintenanceMargin(tiers, 2_000_000n * USDC)).toBe(30_000n * USDC);
  });
  it('isolated liquidation price: 0.5 BTC long from 66,800 with 1,000 USDC, rounded up to the tick', () => {
    // (33,400 - 1,000) / (0.5 x 0.99) = 65,454.54...; the tick is 0.5, a long rounds up: 65,455.0.
    expect(liquidationPrice({ size: SZ / 2n, entryPx: 66_800n * PX, backing: 1_000n * USDC, tiers, tick: PX / 2n })).toBe(65_455n * PX);
    // A short rounds down: (33,400 + 1,000) / (0.5 x 1.01) = 68,118.81...: 68,118.5.
    expect(liquidationPrice({ size: -SZ / 2n, entryPx: 66_800n * PX, backing: 1_000n * USDC, tiers, tick: PX / 2n })).toBe(68_118_50_000_000n);
  });
  it('funding: a long pays at a positive rate; the payment is rounded against the payer', () => {
    expect(fundingPayment(SZ / 2n, 67_000n * PX, 1_250n)).toBe(-418_750n);
    expect(fundingPayment(-SZ / 2n, 67_000n * PX, 1_250n)).toBe(418_750n);
    expect(fundingPayment(SZ / 3n, 67_000n * PX, 1_250n)).toBe(-279_167n);
  });
  it('size for a budget never exceeds it', () => {
    const size = sizeForBudget({ budget: 1_000n * USDC, px: 50_000n * PX, leverage: 10, feePpm: 450n, step: 10_000n });
    expect(size).toBe(19_910_000n);
    const n = notional(size, 50_000n * PX, 'ceil');
    expect(initialMargin(n, 10) + fee(n, 450n)).toBeLessThanOrEqual(1_000n * USDC);
  });
  it('account summary: cross equity, isolated margin set aside, margin ratio rounded up', () => {
    const s = accountSummary(25_000n * USDC, [
      { market: 'BTC-USD', size: SZ / 2n, entryPx: 66_800n * PX, markPx: 67_000n * PX, leverage: 10, isolated: false, isolatedMargin: 0n, tiers, tick: PX / 2n },
    ]);
    expect(s.equity).toBe(25_100n * USDC);
    expect(s.available).toBe(25_100n * USDC - 3_350n * USDC);
    expect(s.marginRatioPpm).toBe(mulDiv(335n * USDC, 1_000_000n, 25_100n * USDC, 'ceil'));
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
