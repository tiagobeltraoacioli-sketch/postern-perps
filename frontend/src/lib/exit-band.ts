/**
 * The exit band: how a position is priced when it leaves a HALTED market (docs/SPEC.md 12 and 14, engine 0.2.3,
 * review finding R436-18). UNAUDITED prototype.
 *
 * In a halt there is no trusted price. A forced close, and the value of a position in escape mode, are therefore
 * priced at the side of a band around the last index that is WORSE for the account: a long below it, a short above
 * it. The band starts `exitHaircutBps` wide, widens by `exitHaircutPerMinBps` for each minute the halt lasts, up to
 * `exitMaxHaircutBps`, and never goes past the price at which the position is worth nothing.
 *
 * The figures are read from compliance/markets.json (the launch table the engine's genesis is built from); nothing
 * here is typed in. The exchange API does not publish these fields today (asked of the node, F434-6): until it does
 * the page states them as "the launch table's" and a test ties this file to that table.
 */
import launch from '../../../compliance/markets.json';
import { formatFixed, formatDuration } from './format';

export type ExitBand = { symbol: string; baseBps: number; perMinuteBps: number; maxBps: number; maxHaltMs: number };

const read = (m: (typeof launch.markets)[number]): ExitBand | null => {
  const o = m.oracle as Record<string, unknown>;
  const n = (k: string) => (typeof o[k] === 'number' && Number.isInteger(o[k]) && (o[k] as number) >= 0 ? (o[k] as number) : null);
  const base = n('exitHaircutBps');
  const per = n('exitHaircutPerMinBps');
  const max = n('exitMaxHaircutBps');
  const halt = n('maxHaltMs');
  if (base === null || per === null || max === null || halt === null || max >= 10_000 || base > max) return null;
  return { symbol: m.symbol, baseBps: base, perMinuteBps: per, maxBps: max, maxHaltMs: halt };
};
/** Every market of the launch table that states its exit band. */
export const EXIT_BANDS: readonly ExitBand[] = launch.markets.map(read).filter((b): b is ExitBand => b !== null);
export const exitBand = (symbol: string): ExitBand | null => EXIT_BANDS.find((b) => b.symbol === symbol) ?? null;

/** The band's width after `minutes` of halt, in basis points: base + per minute, never above the maximum. */
export function haircutBps(band: ExitBand, minutes: number): number {
  const m = Math.max(0, Math.floor(Number.isFinite(minutes) ? minutes : 0));
  return Math.min(band.maxBps, band.baseBps + band.perMinuteBps * m);
}
/**
 * The price a position would leave at, from the index (price scale), `minutes` into a halt: a long at the low side
 * (rounded down), a short at the high side (rounded up). An estimate: the engine's band is taken from every index
 * since the halt and only widens, and the close never goes past the position's bankruptcy price.
 */
export function exitPrice(band: ExitBand, index: bigint, long: boolean, minutes: number): bigint {
  const bps = BigInt(haircutBps(band, minutes));
  return long ? (index * (10_000n - bps)) / 10_000n : (index * (10_000n + bps) + 9_999n) / 10_000n;
}
const pct = (bps: number) => `${formatFixed(BigInt(bps), 2, { dp: 2, trim: true })}%`;
/** "BTC-USD 5% at once, 1% more per minute, up to 50%". */
export function bandWords(band: ExitBand): string {
  return `${band.symbol} ${pct(band.baseBps)} at once, ${pct(band.perMinuteBps)} more per minute of halt, up to ${pct(band.maxBps)}`;
}
/** One sentence for every market of the table, for screens that cannot know which market is meant. */
export function allBandsWords(): string {
  if (EXIT_BANDS.length === 0) return 'The size of that discount is fixed per market in the exchange\'s launch table.';
  return `By the launch table: ${EXIT_BANDS.map(bandWords).join('; ')}.`;
}
/** A halt longer than this settles the market (SPEC X3): stated beside the band. */
export const haltLimitWords = (band: ExitBand) => formatDuration(band.maxHaltMs);

/** The sentence every forced-close screen and review carries (R436-18). */
export const FORCED_CLOSE_WORDS =
  'A forced close goes through the order book while the market trades. In a HALTED market there is no trusted price: the position is closed at the side of the exit band that is worse for you (a long below the last index, a short above it), a discount that grows while the halt lasts and never goes past the price at which the position is worth nothing. It can cost the whole margin of a leveraged position.';
/** How positions are valued when the vault pays in escape mode (SPEC 11, escape values). */
export const ESCAPE_VALUATION_WORDS =
  'positions valued at the last mark price, or in a halted market at the side of the exit band that is worse for the account';
