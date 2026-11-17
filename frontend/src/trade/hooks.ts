/** Small read hooks over the feed's stores. */
import { useEffect, useState } from 'react';
import { feed } from '../state/session';
import { Store, useStore } from '../lib/store';
import type { Market, Ticker } from '../lib/types';
import type { PositionInput } from '../lib/calc';
import type { AccountState } from '../lib/feed';

export function useMarket(symbol: string): Market | undefined {
  return useStore(feed.markets).find((m) => m.symbol === symbol);
}
export function useTicker(symbol: string): Ticker | undefined {
  return useStore(feed.tickers).get(symbol);
}
export function useAccountState(): AccountState | null {
  return useStore(feed.account);
}

/** A price picked in the book, the trades or the chart: the order form takes it as its limit price. */
export const pricePick = new Store<{ text: string; at: number } | null>(null);

/** A clock that ticks every `ms` (countdowns, ages). */
export function useNow(ms = 1000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), ms);
    return () => clearInterval(id);
  }, [ms]);
  return now;
}

/** The account's positions as calc.ts inputs (tiers and tick from the published market). */
export function positionInputs(state: AccountState | null, markets: Market[], tickers: ReadonlyMap<string, Ticker>): PositionInput[] {
  if (!state) return [];
  return state.account.positions.flatMap((p) => {
    const m = markets.find((x) => x.symbol === p.symbol);
    if (!m) return [];
    return [
      {
        market: p.symbol,
        size: p.size,
        entryPx: p.entryPx,
        markPx: tickers.get(p.symbol)?.mark ?? p.markPx,
        leverage: p.leverage,
        isolated: p.isolated,
        isolatedMargin: p.isolatedMargin,
        tiers: m.tiers,
        tick: m.tick,
      },
    ];
  });
}
