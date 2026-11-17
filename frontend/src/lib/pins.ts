/**
 * The market table this interface signs against, PINNED in config/markets.json. UNAUDITED prototype.
 *
 * Why: a signed PlaceOrder names its market by `marketId` and its price and size as scaled integers. If the one API
 * this page reads said BTC-USD is market 2, or that its price has another number of decimals, the user would sign an
 * order on another market or at another price while the review (resolved through the same table) looked right. So
 * the fields that change what a signature means are compared with the committed table, and a market whose published
 * values differ is refused for signing (`signingProblem`). Fields that only change an estimate (fees, tiers, leverage)
 * are taken from the exchange and a difference from the pin is reported, not refused.
 */
import pinned from '../../config/markets.json';
import type { Market } from './types';
import { parseFixed, PX_DECIMALS, SZ_DECIMALS } from './fixed';

export type PinnedMarket = (typeof pinned.markets)[number];
export const PINNED_MARKETS: readonly PinnedMarket[] = pinned.markets;

/** Fields whose value changes what a signature means. */
export const SIGNING_FIELDS = ['marketId', 'base', 'priceDecimals', 'sizeDecimals', 'tick', 'step'] as const;

export function pinFor(symbol: string): PinnedMarket | undefined {
  return PINNED_MARKETS.find((m) => m.symbol === symbol);
}

/** The first difference between a published market and the pin that would change a signature, or null. */
export function signingProblem(market: Market): string | null {
  const pin = pinFor(market.symbol);
  if (!pin) return `${market.symbol} is not in this interface's market table`;
  if (pin.marketId !== market.marketId) return `${market.symbol}: the exchange says market id ${market.marketId}, this interface has ${pin.marketId}`;
  if (pin.base !== market.base) return `${market.symbol}: the base asset differs`;
  if (pin.priceDecimals !== market.priceDecimals || pin.sizeDecimals !== market.sizeDecimals) return `${market.symbol}: the number of decimals differs`;
  if (parseFixed(pin.tickSize, PX_DECIMALS) !== market.tick) return `${market.symbol}: the price tick differs`;
  if (parseFixed(pin.stepSize, SZ_DECIMALS) !== market.step) return `${market.symbol}: the size step differs`;
  // Another market claiming this id is as bad as this one being renumbered.
  return null;
}

/** Problems across the whole published table: renumbering, a duplicate id, a market the pin does not know. */
export function tableProblems(markets: Market[]): Map<string, string> {
  const out = new Map<string, string>();
  const ids = new Map<number, string>();
  for (const m of markets) {
    const problem = signingProblem(m);
    if (problem) out.set(m.symbol, problem);
    const other = ids.get(m.marketId);
    if (other) {
      out.set(m.symbol, `${m.symbol} and ${other} both claim market id ${m.marketId}`);
      out.set(other, `${m.symbol} and ${other} both claim market id ${m.marketId}`);
    }
    ids.set(m.marketId, m.symbol);
  }
  return out;
}
