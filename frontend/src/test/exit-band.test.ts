/**
 * R436-18: a forced close and the escape valuation are NOT "at the market" in a halted market. Every screen that
 * names them states the real rule with the figures of the published market table, never typed in.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { Hex } from 'viem';
import launch from '../../../compliance/markets.json';
import { allBandsWords, bandWords, ESCAPE_VALUATION_WORDS, EXIT_BANDS, exitBand, exitPrice, FORCED_CLOSE_WORDS, haircutBps } from '../lib/exit-band';
import { buildTx, describeTx, type Deployment } from '../vault/vault';

const d: Deployment = { chainId: 700770, vault: `0x${'aa'.repeat(20)}` as Hex, token: `0x${'bb'.repeat(20)}` as Hex };
const PX = 10n ** 8n;

describe('exit band figures come from compliance/markets.json', () => {
  it('one band per launch market, field for field', () => {
    expect(EXIT_BANDS.map((b) => b.symbol)).toEqual(launch.markets.map((m) => m.symbol));
    for (const m of launch.markets) {
      const o = m.oracle as Record<string, number>;
      expect(exitBand(m.symbol)).toEqual({ symbol: m.symbol, baseBps: o.exitHaircutBps, perMinuteBps: o.exitHaircutPerMinBps, maxBps: o.exitMaxHaircutBps, maxHaltMs: o.maxHaltMs });
    }
    expect(exitBand('DOGE-USD')).toBeNull();
  });
  it('the band widens per whole minute and stops at its maximum', () => {
    for (const b of EXIT_BANDS) {
      expect(haircutBps(b, 0)).toBe(b.baseBps);
      expect(haircutBps(b, 0.99)).toBe(b.baseBps);
      expect(haircutBps(b, 3)).toBe(Math.min(b.maxBps, b.baseBps + 3 * b.perMinuteBps));
      expect(haircutBps(b, 100_000)).toBe(b.maxBps);
      expect(haircutBps(b, -5)).toBe(b.baseBps);
      expect(haircutBps(b, Number.NaN)).toBe(b.baseBps);
    }
  });
  it('a long leaves below the index (rounded down), a short above it (rounded up): always the worse side', () => {
    const b = EXIT_BANDS[0];
    const index = 67_000n * PX + 1n;
    const low = exitPrice(b, index, true, 0);
    const high = exitPrice(b, index, false, 0);
    expect(low).toBe((index * BigInt(10_000 - b.baseBps)) / 10_000n);
    expect(low < index && high > index).toBe(true);
    expect(high * 10_000n >= index * BigInt(10_000 + b.baseBps)).toBe(true);
    expect((high - 1n) * 10_000n < index * BigInt(10_000 + b.baseBps)).toBe(true);
    expect(exitPrice(b, index, true, 10) < low).toBe(true);
  });
  it('the words carry every market\'s own numbers', () => {
    for (const b of EXIT_BANDS) {
      const w = bandWords(b);
      expect(w).toContain(b.symbol);
      expect(w).toContain(`${b.baseBps / 100}% at once`);
      expect(w).toContain(`up to ${b.maxBps / 100}%`);
      expect(allBandsWords()).toContain(w);
    }
  });
});

describe('what the screens say (the old wording must be gone)', () => {
  it('the review of a forced close states the rule and the figures; a forced withdrawal does not carry them', () => {
    const close = describeTx(d, buildTx(d, { kind: 'requestForced', forced: 'closeAll', arg: 0n })).notes.join(' ');
    expect(close).toContain(FORCED_CLOSE_WORDS);
    expect(close).toContain(allBandsWords());
    expect(close).toMatch(/worse for you/);
    expect(close).not.toMatch(/at the market/);
    const withdraw = describeTx(d, buildTx(d, { kind: 'requestForced', forced: 'withdraw', arg: 0n })).notes.join(' ');
    expect(withdraw).not.toContain(allBandsWords());
  });
  it('the escape valuation is stated with the halted-market rule', () => {
    expect(ESCAPE_VALUATION_WORDS).toMatch(/mark price/);
    expect(ESCAPE_VALUATION_WORDS).toMatch(/halted market.*exit band.*worse for the account/);
  });
  it('no screen still says a forced close is "at the market" or that escape closes "at the last mark price"', () => {
    const read = (p: string) => readFileSync(resolve(process.cwd(), p), 'utf8');
    const funds = read('src/pages/Funds.tsx');
    const home = read('src/pages/Home.tsx');
    const vault = read('src/vault/vault.ts');
    expect(funds).not.toContain('close every position of this account at the market');
    expect(funds).not.toContain('positions closed at the last mark price');
    expect(home).not.toContain('positions closed at the last mark price');
    expect(vault).not.toContain('at the market');
    // Each of them takes its sentence from lib/exit-band.ts.
    expect(funds).toContain('ESCAPE_VALUATION_WORDS');
    expect(funds).toContain('FORCED_CLOSE_WORDS');
    expect(home).toContain('ESCAPE_VALUATION_WORDS');
    expect(vault).toContain('FORCED_CLOSE_WORDS');
  });
});
