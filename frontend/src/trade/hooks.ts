/** Small read hooks over the feed's stores. */
import { useEffect, useState, type RefObject } from 'react';
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

/** The side the order form is on (the phone's Long / Short bar sets it before opening the form). */
export const orderSide = new Store<'buy' | 'sell'>('buy');

/** True while the viewport matches `query` (layout decisions that CSS alone cannot make: inert, focus). */
export function useMedia(query: string): boolean {
  const [matches, setMatches] = useState(() => typeof matchMedia === 'function' && matchMedia(query).matches);
  useEffect(() => {
    if (typeof matchMedia !== 'function') return;
    const m = matchMedia(query);
    const on = () => setMatches(m.matches);
    on();
    m.addEventListener('change', on);
    return () => m.removeEventListener('change', on);
  }, [query]);
  return matches;
}

/**
 * True while the element `ref` scrolls and has content below its visible part. The order gate uses it to fade the
 * last lines of its body into the action bar, so a cut line never looks like the end of the form.
 */
export function useScrollMore(ref: RefObject<HTMLElement | null>): boolean {
  const [more, setMore] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setMore(el.scrollHeight - el.scrollTop - el.clientHeight > 2);
    measure();
    el.addEventListener('scroll', measure, { passive: true });
    const ro = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(measure) : null;
    ro?.observe(el);
    const mo = typeof MutationObserver !== 'undefined' ? new MutationObserver(measure) : null;
    mo?.observe(el, { childList: true, subtree: true, characterData: true });
    return () => {
      el.removeEventListener('scroll', measure);
      ro?.disconnect();
      mo?.disconnect();
    };
  }, [ref]);
  return more;
}

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
        openNotional: p.openNotional ?? undefined,
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
