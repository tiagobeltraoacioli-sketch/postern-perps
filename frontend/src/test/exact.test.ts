/**
 * lib/exact.ts against the ENGINE's worked examples (engine/vectors/worked-examples.json, agent 430, engine cd8e5ae,
 * SPEC 0.2.2): every value of every case. The copy under src/test/vectors is compared byte for byte with the file
 * in engine/ whenever that file is in the tree, so the two cannot drift once main carries it.
 */
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import v from './vectors/worked-examples.json';
import {
  bankruptcyPriceOf,
  equityOf,
  feeOf,
  fundingCreditOf,
  fundingRateOf,
  initialReq,
  leverageFractionPpm,
  liquidatable,
  liquidationBoundOf,
  liquidationFeeOf,
  liquidationPriceOf,
  maintenanceOf,
  maintenanceReq,
  markOf,
  notionalOf,
  premiumOf,
  unrealisedOf,
  usdcCeil,
  usdcFloor,
  type Tier,
} from '../lib/exact';

const b = (s: string) => BigInt(s);
const nb = (s: string | null) => (s === null ? null : BigInt(s));
const tiersOf = (name: string): Tier[] => v.tierSets.find((t) => t.name === name)!.tiers.map((t) => ({ floor: b(t.floor), imfPpm: b(t.imfPpm), mmfPpm: b(t.mmfPpm) }));
let values = 0;
const eq = (got: bigint | boolean | null, want: string | boolean | null, what: string) => {
  values++;
  expect(got === null || typeof got === 'boolean' ? got : got.toString(), what).toBe(want);
};

describe('engine worked examples (SPEC 0.2.2, 430)', () => {
  it('the copy is the engine\'s file (sha256 pinned; byte-equal to engine/vectors when present)', () => {
    const copy = readFileSync(resolve(process.cwd(), 'src/test/vectors/worked-examples.json'));
    expect(createHash('sha256').update(copy).digest('hex')).toBe('6695161855eeb4b8120863d9a46b4665ee4cd40fa47d4c8cf8fb829277a6ed0f');
    const inTree = resolve(process.cwd(), '../engine/vectors/worked-examples.json');
    if (existsSync(inTree)) expect(readFileSync(inTree).equals(copy), 'engine/vectors/worked-examples.json changed: copy it again and re-run').toBe(true);
    expect(v.specVersion).toBe('0.2');
  });
  it('scales', () => {
    expect(v.scales).toMatchObject({ NOTIONAL_PER_USDC: '10000000000', PPM: '1000000', RATE_SCALE: '1000000000', PRICE_SCALE: '100000000', SIZE_SCALE: '100000000', USDC_SCALE: '1000000', MAX_PRICE: '1000000000000000' });
  });
  it(`fees: ${v.fees.length} cases`, () => {
    for (const c of v.fees) {
      eq(notionalOf(b(c.size), b(c.price)), c.notional, 'notional');
      eq(feeOf(b(c.notional), b(c.feePpm)), c.fee, `fee of ${c.notional} at ${c.feePpm}`);
    }
  });
  it(`margin: ${v.margin.length} cases (both tier sets, tier crossings, the leverage fraction)`, () => {
    for (const c of v.margin) {
      const tiers = tiersOf(c.tiers);
      const n = notionalOf(b(c.size), b(c.price));
      eq(n, c.notional, 'notional');
      eq(leverageFractionPpm(Number(c.leverage)), c.leverageFractionPpm, `leverage fraction ${c.leverage}`);
      eq(initialReq(tiers, n, Number(c.leverage)), c.initialReqNotional, 'initial requirement');
      eq(maintenanceReq(tiers, n), c.maintenanceReqNotional, 'maintenance requirement');
      eq(usdcCeil(b(c.initialReqNotional)), c.initialMargin, 'initial margin USDC');
      eq(usdcCeil(b(c.maintenanceReqNotional)), c.maintenanceMargin, 'maintenance margin USDC');
    }
  });
  it(`liquidation price: ${v.liquidationPrice.length} cases, each with the engine's evaluation at the price and one unit on the safe side`, () => {
    for (const c of v.liquidationPrice) {
      const tiers = tiersOf(c.tiers);
      const position = (mark: bigint) => [{ size: b(c.size), mark, openNotional: b(c.openNotional), tiers }];
      eq(b(c.size) * b(c.entryPrice), c.openNotional, 'open notional = size x entry price');
      const got = liquidationPriceOf(b(c.balance), position(b(c.entryPrice)), 0);
      eq(got, c.liquidationPrice, `liquidation price of ${c.size} from ${c.entryPrice} with ${c.balance}`);
      for (const point of [c.atLiquidationPrice, c.oneUnitOnTheSafeSide]) {
        if (!point) continue;
        const p = position(b(point.mark));
        eq(equityOf(b(c.balance), p), point.equityNotional, 'equity');
        eq(maintenanceOf(p), point.maintenanceReqNotional, 'maintenance');
        eq(liquidatable(b(c.balance), p), point.liquidatable, 'liquidatable');
      }
    }
  });
  it(`liquidation execution: ${v.liquidationExecution.length} cases (bankruptcy price, bound, fee before the cap)`, () => {
    for (const c of v.liquidationExecution) {
      const bp = bankruptcyPriceOf(b(c.size), b(c.openNotional), b(c.restNotional));
      eq(bp, c.bankruptcyPrice, 'bankruptcy price');
      eq(liquidationBoundOf(b(c.size), bp, b(c.mark), b(c.mmf0Ppm)), c.liquidationBound, 'bound');
      eq(liquidationFeeOf(b(c.size), b(c.mark), b(c.liquidationFeePpm)), c.liquidationFeeBeforeCap, 'fee before cap');
    }
  });
  it(`funding rate: ${v.fundingRate.length} cases (median, clamp, cap, none without samples)`, () => {
    for (const c of v.fundingRate) eq(fundingRateOf(c.premiumSamples.map(b), b(c.interest), b(c.fundingClamp), b(c.maxFundingRate)), c.rate, `rate of [${c.premiumSamples}]`);
  });
  it(`funding payment: ${v.fundingPayment.length} cases`, () => {
    for (const c of v.fundingPayment) eq(fundingCreditOf(b(c.size), b(c.mark), b(c.rate)), c.credit, `credit of ${c.size} at ${c.rate}`);
  });
  it(`premium: ${v.premium.length} cases; mark: ${v.mark.length} cases`, () => {
    for (const c of v.premium) eq(premiumOf(b(c.index), nb(c.impactBid), nb(c.impactAsk)), c.premium, 'premium');
    for (const c of v.mark) eq(markOf(b(c.index), nb(c.bestBid), nb(c.bestAsk), nb(c.freshLastTrade), b(c.markBandBps)), c.mark, 'mark');
  });
  it(`pnl: ${v.pnl.length} cases (exact in notional units; floored to USDC for gains and losses alike)`, () => {
    for (const c of v.pnl) {
      const u = unrealisedOf(b(c.size), b(c.mark), b(c.openNotional));
      eq(u, c.unrealisedNotional, 'unrealised');
      eq(usdcFloor(u), c.unrealisedUsdcTowardProtocol, 'unrealised USDC');
    }
  });
  it('every value of the file was compared: 755', () => {
    // Counted by this file's own comparisons (the suite runs in order in one worker).
    expect(values).toBe(755);
  });
});
