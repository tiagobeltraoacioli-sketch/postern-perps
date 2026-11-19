/** The pinned market table (config/markets.json) against the launch table of compliance/markets.json: the fields
 *  that change what a signature means must be equal, market by market, and neither side may have a market the other
 *  lacks. A change of the compliance table therefore stops this build's tests until the pin is regenerated on purpose. */
import { describe, expect, it } from 'vitest';
import compliance from '../../../compliance/markets.json';
import pinned from '../../config/markets.json';

describe('market pin = compliance/markets.json (signing fields)', () => {
  it('same markets, same ids, bases, decimals, ticks and steps', () => {
    expect(pinned.markets.map((m) => m.symbol)).toEqual(compliance.markets.map((m) => m.symbol));
    for (const c of compliance.markets) {
      const p = pinned.markets.find((m) => m.symbol === c.symbol)!;
      expect({ marketId: p.marketId, base: p.base, priceDecimals: p.priceDecimals, sizeDecimals: p.sizeDecimals, tick: p.tickSize, step: p.stepSize }).toEqual({
        marketId: c.marketId,
        base: c.base,
        priceDecimals: c.priceDecimals,
        sizeDecimals: c.sizeDecimals,
        tick: c.tick,
        step: c.step,
      });
    }
    // The pin names the table it was generated from; a later version of that table that changes no signing field
    // (435's 0.2.2: oracle exit-band fields only) needs no new pin, so the version itself is not compared.
    expect(pinned.source).toMatch(/^compliance\/markets\.json \d+\.\d+\.\d+$/);
    expect(compliance.version).toMatch(/^\d+\.\d+\.\d+$/);
  });
  it('the display precision equals the precision of the tick and the step (SPEC 1)', () => {
    const places = (s: string) => (s.includes('.') ? s.split('.')[1].length : 0);
    for (const m of pinned.markets) {
      expect(places(m.tickSize), m.symbol).toBe(m.priceDecimals);
      expect(places(m.stepSize), m.symbol).toBe(m.sizeDecimals);
    }
  });
});
