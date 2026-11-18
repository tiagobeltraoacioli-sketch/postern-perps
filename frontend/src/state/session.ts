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
import { compareWithNode, DOMAIN, domainFor, type Domain } from '../lib/eip712';
import { LOCAL_RENDER } from '../lib/config';
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
/**
 * The signing domain. A published build takes it ONLY from config/production.json (lib/eip712.ts DOMAIN). A local
 * render build against a loopback mock whose config names no vault takes the vault from the mock's /v1/status once
 * (393's reference mock deploys its own); such a build is marked "not for publication" and fails the bundle gate.
 */
export const signingDomain = new Store<Domain | null>(DOMAIN);

export async function checkEip712() {
  try {
    const published = await api.get<unknown>('/v1/eip712');
    eip712Problem.set(compareWithNode(published, signingDomain.get()));
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
    if (LOCAL_RENDER && !DOMAIN && !signingDomain.get() && s) {
      const local = domainFor(s.network?.vault);
      if (local) {
        signingDomain.set(local);
        void checkEip712();
      }
    }
  });
}

/** The guard of lib/sign.ts from the current stores. Also usable outside React (`currentGuard`). */
export function currentGuard(account: string | null, walletChainId?: string | null): Guard {
  const markets = feed.markets.get();
  return {
    walletChainId,
    domain: signingDomain.get(),
    account: account?.toLowerCase() ?? null,
    eip712Problem: eip712Problem.get(),
    markets,
    tableProblems: tableProblems(markets),
    geo: geo.get(),
    status: feed.status.get(),
    now: Date.now(),
    clockSkewMs: clockSkewMs.get(),
  };
}
export function useGuard(account: string | null): Guard {
  const problem = useStore(eip712Problem);
  const g = useStore(geo);
  const markets = useStore(feed.markets);
  const status = useStore(feed.status);
  const domain = useStore(signingDomain);
  const skew = useStore(clockSkewMs);
  return useMemo(
    () => ({ domain, account: account?.toLowerCase() ?? null, eip712Problem: problem, markets, tableProblems: tableProblems(markets), geo: g, status, now: Date.now(), clockSkewMs: skew }),
    [domain, account, problem, g, markets, status, skew],
  );
}

/* ---------- Route (hash based: works from any static host and from file://) ---------- */
export type FundsTab = 'deposit' | 'withdraw' | 'exit';
export type HistoryTab = 'orders' | 'fills' | 'funding' | 'transfers';
export type Route =
  | { page: 'trade'; symbol: string }
  | { page: 'transfers'; tab: FundsTab }
  | { page: 'history'; tab: HistoryTab }
  | { page: 'funding'; symbol: string }
  | { page: 'markets' | 'portfolio' | 'agents' | 'system' | 'legal' }
  /** An address this application does not have: the page says so (it never silently shows another page). */
  | { page: 'notfound'; path: string };
export const PAGES = ['trade', 'markets', 'portfolio', 'transfers', 'history', 'funding', 'agents', 'system', 'legal'] as const;
const SYMBOL = /^[A-Z0-9]{2,10}-USD$/;
const FUNDS_TABS: readonly string[] = ['deposit', 'withdraw', 'exit'];
const HISTORY_TABS: readonly string[] = ['orders', 'fills', 'funding', 'transfers'];
export function parseRoute(hash: string): Route {
  const path = hash.replace(/^#\/?/, '');
  const parts = path.split('/');
  const page = parts[0] || 'trade';
  const notfound: Route = { page: 'notfound', path: path.slice(0, 80) };
  if (page === 'trade') {
    if (parts.length > 2) return notfound;
    if (parts[1] === undefined || parts[1] === '') return { page: 'trade', symbol: 'BTC-USD' };
    return SYMBOL.test(parts[1]) ? { page: 'trade', symbol: parts[1] } : notfound;
  }
  // "funds" is the page's name on screen; "transfers" is its first address and stays valid.
  if (page === 'transfers' || page === 'funds') {
    if (parts.length > 2) return notfound;
    if (parts[1] === undefined || parts[1] === '') return { page: 'transfers', tab: 'deposit' };
    return FUNDS_TABS.includes(parts[1]) ? { page: 'transfers', tab: parts[1] as FundsTab } : notfound;
  }
  if (page === 'history') {
    if (parts.length > 2) return notfound;
    if (parts[1] === undefined || parts[1] === '') return { page: 'history', tab: 'orders' };
    return HISTORY_TABS.includes(parts[1]) ? { page: 'history', tab: parts[1] as HistoryTab } : notfound;
  }
  if (page === 'funding') {
    if (parts.length > 2) return notfound;
    if (parts[1] === undefined || parts[1] === '') return { page: 'funding', symbol: 'BTC-USD' };
    return SYMBOL.test(parts[1]) ? { page: 'funding', symbol: parts[1] } : notfound;
  }
  if (parts.length === 1 && (['markets', 'portfolio', 'agents', 'system', 'legal'] as string[]).includes(page)) return { page } as Route;
  return notfound;
}
export function routeHash(r: Route): string {
  switch (r.page) {
    case 'trade':
      return `#/trade/${r.symbol}`;
    case 'transfers':
      return `#/transfers/${r.tab}`;
    case 'history':
      return `#/history/${r.tab}`;
    case 'funding':
      return `#/funding/${r.symbol}`;
    case 'notfound':
      return `#/${r.path}`;
    default:
      return `#/${r.page}`;
  }
}
export const route = new Store<Route>(typeof location !== 'undefined' ? parseRoute(location.hash) : { page: 'trade', symbol: 'BTC-USD' });
if (typeof window !== 'undefined') window.addEventListener('hashchange', () => route.set(parseRoute(location.hash)));
export function go(r: Route) {
  const hash = routeHash(r);
  if (location.hash !== hash) location.hash = hash;
  else route.set(r);
}
