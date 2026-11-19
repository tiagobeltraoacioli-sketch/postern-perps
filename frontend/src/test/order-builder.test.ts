import { describe, expect, it } from 'vitest';
import { buildOrder, parseAt, worstPrice, DEFAULT_SLIPPAGE_PPM, type OrderForm } from '../trade/order-builder';
import { market, NOW } from './fixtures';
import { PX } from '../lib/fixed';

const btc = market('BTC-USD'); // price dp 1, tick 0.5, size dp 4, step 0.0001
const account = '0x1111111111111111111111111111111111111111';
const form = (over: Partial<OrderForm> = {}): OrderForm => ({ side: 'buy', kind: 'limit', size: '0.25', price: '67000.5', trigger: '', tif: 'GTC', reduceOnly: false, slippagePpm: DEFAULT_SLIPPAGE_PPM, ...over });
const build = (f: Partial<OrderForm>, mark: bigint | null = 67_000n * PX, levels = [{ px: 67_010n * PX, size: 10n ** 8n }]) =>
  buildOrder({ form: form(f), market: btc, mark, levels, account, nonce: BigInt(NOW), clientOrderId: `0x${'00'.repeat(32)}` });

describe('parseAt', () => {
  it('parses at the market decimals into the engine scale and never rounds', () => {
    expect(parseAt('67000.5', 1, 8).value).toBe(67_000_50_000_000n);
    expect(parseAt('67000.55', 1, 8).error).toMatch(/at most 1 decimal/);
    expect(parseAt('1,000', 1, 8).value).toBe(1000n * PX);
    expect(parseAt('-1', 1, 8).error).toMatch(/positive/);
    expect(parseAt('abc', 1, 8).error).toMatch(/not a number/);
    expect(parseAt('', 1, 8)).toEqual({ value: null, error: null });
  });
});

describe('worstPrice', () => {
  it('moves the reference by the bound and rounds to the tick in the user favour', () => {
    // 67,000 x 1.01 = 67,670 exactly; a buy bound rounds down, a sell bound up.
    expect(worstPrice(67_000n * PX, true, 10_000n, btc.tick)).toBe(67_670n * PX);
    expect(worstPrice(67_000n * PX, false, 10_000n, btc.tick)).toBe(66_330n * PX);
    // 67,000.3 x 1.01 = 67,670.303: down to 67,670.0 for a buy; x 0.99 = 66,330.297: up to 66,330.5 for a sell.
    expect(worstPrice(67_000_30_000_000n, true, 10_000n, btc.tick)).toBe(67_670n * PX);
    expect(worstPrice(67_000_30_000_000n, false, 10_000n, btc.tick)).toBe(66_330_50_000_000n);
  });
});

describe('buildOrder', () => {
  it('a limit order signs exactly the typed values', () => {
    const b = build({});
    expect(b.errors).toEqual([]);
    expect(b.message).toMatchObject({ account, marketId: 1, isBuy: true, orderType: 0, timeInForce: 0, price: '6700050000000', size: '25000000', reduceOnly: false, triggerPrice: '0' });
  });
  it('a market order carries its worst price and is IOC, never price 0', () => {
    const b = build({ kind: 'market', tif: 'GTC' });
    expect(b.message?.price).toBe((67_670n * PX).toString());
    expect(b.message?.timeInForce).toBe(1);
    expect(b.message?.orderType).toBe(1);
  });
  it('a market order without a mark price is not built', () => {
    const b = build({ kind: 'market' }, null);
    expect(b.message).toBeNull();
    expect(b.errors.join()).toMatch(/No mark price/);
  });
  it('the bound must be 0.01% to 10%', () => {
    expect(build({ kind: 'market', slippagePpm: 0n }).errors.join()).toMatch(/between/);
    expect(build({ kind: 'market', slippagePpm: 100_001n }).errors.join()).toMatch(/between/);
  });
  it('warns when the visible book does not fill inside the bound', () => {
    const b = build({ kind: 'market', size: '2' });
    expect(b.warnings.join()).toMatch(/Only part/);
    expect(b.estimate?.filled).toBe(10n ** 8n);
    const none = build({ kind: 'market' }, 67_000n * PX, [{ px: 70_000n * PX, size: 10n ** 9n }]);
    expect(none.warnings.join()).toMatch(/would not fill/);
  });
  it('a stop-limit built while the form still holds IOC or post-only is sent good-till-cancelled (engine 0.2.3)', () => {
    for (const tif of ['IOC', 'POST_ONLY', 'GTC'] as const) {
      const b = build({ kind: 'stopLimit', trigger: '66000', price: '65900', tif });
      expect(b.errors).toEqual([]);
      expect(b.message!.timeInForce).toBe(0);
    }
    expect(build({ kind: 'limit', tif: 'IOC' }).message!.timeInForce).toBe(1);
  });
  it('price off tick, size off step, too many places: errors, no message', () => {
    expect(build({ price: '67000.3' }).errors.join()).toMatch(/multiple of 0.5/);
    expect(build({ size: '0.00015' }).errors.join()).toMatch(/at most 4 decimal/);
    expect(build({ size: '0' }).errors.join()).toMatch(/above zero/);
    // The bound is the market table's (compliance/markets.json 0.2.1: 10 BTC; the round-1 fixture said 50).
    expect(build({ size: '51' }).errors.join()).toMatch(/largest order is 10 BTC/);
    expect(build({ size: '10.0001' }).errors.join()).toMatch(/largest order is 10 BTC/);
    expect(build({ size: '10' }).errors.join()).not.toMatch(/largest order/);
    expect(build({ price: '67000.3' }).message).toBeNull();
  });
  it('a stop market order: trigger required, worst price from the trigger, crossed trigger warned', () => {
    expect(build({ kind: 'stopMarket', side: 'sell', trigger: '' }).errors.join()).toMatch(/trigger price/);
    const b = build({ kind: 'stopMarket', side: 'sell', trigger: '65000', reduceOnly: true });
    expect(b.message).toMatchObject({ orderType: 2, timeInForce: 1, triggerPrice: (65_000n * PX).toString(), price: (64_350n * PX).toString(), reduceOnly: true });
    expect(build({ kind: 'stopMarket', side: 'sell', trigger: '68000' }).warnings.join()).toMatch(/trigger at once/);
  });
  it('post-only through the mark warns; a deep-through limit warns', () => {
    expect(build({ tif: 'POST_ONLY', price: '67100' }).warnings.join()).toMatch(/post-only/);
    expect(build({ price: '69000' }).warnings.join()).toMatch(/through the mark/);
  });
  it('no wallet: no message', () => {
    const b = buildOrder({ form: form(), market: btc, mark: 67_000n * PX, levels: [], account: null, nonce: null, clientOrderId: `0x${'00'.repeat(32)}` });
    expect(b.message).toBeNull();
    expect(b.errors.join()).toMatch(/Connect a wallet/);
  });
});
