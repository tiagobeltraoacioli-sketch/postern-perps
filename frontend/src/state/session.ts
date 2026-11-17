/**
 * The page's live session: one feed, the checks every signature depends on, and the route. UNAUDITED prototype.
 *
 *  - `eip712Problem`: undefined until `GET /v1/eip712` has been read and compared with the pin; null when equal;
 *    a sentence when different. Re-read every 5 minutes and after every reconnect, so a node upgrade that changes a
 *    type stops signing within minutes instead of at the next page load.
 *  - `geo`: `GET /v1/geo`, null until read or when unreadable (the signing guard then allows only exit actions).
 *  - `clockSkewMs`: the exchange's `status.time` minus this device's clock when the status was read (nonces are the
 *    millisecond clock; a large skew is shown to the user).
 */
import { useMemo } from 'react';
import { api } from '../lib/api';
import { compareWithNode } from '../lib/eip712';
import { Feed } from '../lib/feed';
import { tableProblems } from '../lib/pins';
import { Store, useStore } from '../lib/store';
import type { Guard } from '../lib/sign';
import type { WireGeo } from '../lib/types';

export const feed = new Feed();
export const eip712Problem = new Store<string | null | undefined>(undefined);
export const geo = new Store<WireGeo | null>(null);
export const geoRead = new Store<'pending' | 'ok' | 'failed'>('pending');
export const clockSkewMs = new Store<number | null>(null);

export async function checkEip712() {
  try {
    const published = await api.get<unknown>('/v1/eip712');
    eip712Problem.set(compareWithNode(published));
  } catch {
    // Unreadable is not "equal": keep the previous verdict if there was one, else stay unchecked.
    if (eip712Problem.get() === undefined) eip712Problem.set(undefined);
  }
}
export async function readGeo() {
  try {
    geo.set(await api.get<WireGeo>('/v1/geo'));
    geoRead.set('ok');
  } catch {
    geo.set(null);
    geoRead.set('failed');
  }
}

let started = false;
export function startSession() {
  if (started) return;
  started = true;
  feed.start();
  void checkEip712();
  void readGeo();
  setInterval(() => void checkEip712(), 300_000);
  setInterval(() => void readGeo(), 600_000);
  let wasOpen = false;
  feed.connection.subscribe(() => {
    const open = feed.connection.get().state === 'open';
    if (open && !wasOpen) void checkEip712();
    wasOpen = open;
  });
  feed.status.subscribe(() => {
    const s = feed.status.get();
    if (s) clockSkewMs.set(s.time - Date.now());
  });
}

/** The guard of lib/sign.ts from the current stores. Also usable outside React (`currentGuard`). */
export function currentGuard(account: string | null): Guard {
  const markets = feed.markets.get();
  return {
    account: account?.toLowerCase() ?? null,
    eip712Problem: eip712Problem.get(),
    markets,
    tableProblems: tableProblems(markets),
    geo: geo.get(),
    status: feed.status.get(),
    now: Date.now(),
  };
}
export function useGuard(account: string | null): Guard {
  const problem = useStore(eip712Problem);
  const g = useStore(geo);
  const markets = useStore(feed.markets);
  const status = useStore(feed.status);
  return useMemo(
    () => ({ account: account?.toLowerCase() ?? null, eip712Problem: problem, markets, tableProblems: tableProblems(markets), geo: g, status, now: Date.now() }),
    [account, problem, g, markets, status],
  );
}

/* ---------- Route (hash based: works from any static host and from file://) ---------- */
export type Route = { page: 'trade'; symbol: string } | { page: 'portfolio' | 'transfers' | 'agents' | 'system' | 'legal' };
export const PAGES = ['trade', 'portfolio', 'transfers', 'agents', 'system', 'legal'] as const;
export function parseRoute(hash: string): Route {
  const parts = hash.replace(/^#\/?/, '').split('/');
  const page = parts[0] || 'trade';
  if (page === 'trade') return { page: 'trade', symbol: /^[A-Z0-9]{2,10}-USD$/.test(parts[1] ?? '') ? parts[1] : 'BTC-USD' };
  if ((PAGES as readonly string[]).includes(page)) return { page: page as Exclude<Route['page'], 'trade'> };
  return { page: 'trade', symbol: 'BTC-USD' };
}
export const route = new Store<Route>(typeof location !== 'undefined' ? parseRoute(location.hash) : { page: 'trade', symbol: 'BTC-USD' });
if (typeof window !== 'undefined') window.addEventListener('hashchange', () => route.set(parseRoute(location.hash)));
export function go(r: Route) {
  const hash = r.page === 'trade' ? `#/trade/${r.symbol}` : `#/${r.page}`;
  if (location.hash !== hash) location.hash = hash;
  else route.set(r);
}
