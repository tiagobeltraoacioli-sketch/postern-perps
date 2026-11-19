/** Addresses of the application. Pass 2 made the bare address the home page: every older link must still open the
 *  page it named, and nothing malformed may open a page at all. */
import { describe, expect, it } from 'vitest';
import { parseRoute, routeHash, type Route } from '../state/session';

describe('routes', () => {
  it('the bare address is the home page; the terminal is #/trade', () => {
    for (const h of ['', '#', '#/']) expect(parseRoute(h)).toEqual({ page: 'home' });
    expect(parseRoute('#/trade')).toEqual({ page: 'trade', symbol: 'BTC-USD' });
    expect(parseRoute('#/trade/')).toEqual({ page: 'trade', symbol: 'BTC-USD' });
    expect(parseRoute('#trade/ETH-USD')).toEqual({ page: 'trade', symbol: 'ETH-USD' });
  });
  it('every link of rounds 1 and 2 still opens its page', () => {
    const old: [string, Route][] = [
      ['#/trade/BTC-USD', { page: 'trade', symbol: 'BTC-USD' }],
      ['#/trade/SOL-USD', { page: 'trade', symbol: 'SOL-USD' }],
      ['#/portfolio', { page: 'portfolio' }],
      ['#/transfers', { page: 'transfers', tab: 'deposit' }],
      ['#/transfers/withdraw', { page: 'transfers', tab: 'withdraw' }],
      ['#/transfers/exit', { page: 'transfers', tab: 'exit' }],
      ['#/funds/exit', { page: 'transfers', tab: 'exit' }],
      ['#/agents', { page: 'agents' }],
      ['#/system', { page: 'system' }],
      ['#/legal', { page: 'legal' }],
      ['#/markets', { page: 'markets' }],
      ['#/history', { page: 'history', tab: 'orders' }],
      ['#/history/fills', { page: 'history', tab: 'fills' }],
      ['#/funding', { page: 'funding', symbol: 'BTC-USD' }],
      ['#/funding/ETH-USD', { page: 'funding', symbol: 'ETH-USD' }],
    ];
    for (const [hash, route] of old) expect(parseRoute(hash), hash).toEqual(route);
  });
  it('a malformed address is the 404 page, never another page', () => {
    for (const bad of ['#/trade/btc-usd', '#/trade/BTC-USD/x', '#/transfers/steal', '#/history/x', '#/portfolio/extra', '#/markets/1', '#/home', '#/nowhere', '#//trade'])
      expect(parseRoute(bad).page, bad).toBe('notfound');
    const long = parseRoute(`#/${'a'.repeat(500)}`);
    expect(long.page === 'notfound' && long.path.length).toBe(80);
  });
  it('an address written from a route reads back as that route', () => {
    const routes: Route[] = [{ page: 'home' }, { page: 'trade', symbol: 'ETH-USD' }, { page: 'transfers', tab: 'exit' }, { page: 'history', tab: 'funding' }, { page: 'funding', symbol: 'SOL-USD' }, { page: 'markets' }, { page: 'legal' }];
    for (const r of routes) expect(parseRoute(routeHash(r))).toEqual(r);
  });
});
