/** Test fixtures: the pinned market table as the exchange would publish it. */
import pinned from '../../config/markets.json';
import * as convert from '../lib/convert';
import type { Market, WireGeo, WireMarket, WireStatus } from '../lib/types';
import type { Guard } from '../lib/sign';

export function wireMarkets(): WireMarket[] {
  return pinned.markets.map((m) => ({
    ...m,
    quote: 'USD',
    status: 'active' as const,
    makerFeeBps: '1.5',
    takerFeeBps: '4.5',
    liquidationFeeBps: '50',
    fundingIntervalSec: 3600,
    maxFundingRate: '0.0005',
    oracle: { sources: ['a', 'b', 'c'], minSources: 3, maxStalenessMs: 10_000, maxDeviationBps: '200' },
    openInterestCap: '1000',
    haltReason: null,
  }));
}
export const markets = (): Market[] => wireMarkets().map(convert.market);
export const market = (symbol: string): Market => markets().find((m) => m.symbol === symbol)!;

export const OPEN_GEO: WireGeo = { country: 'PA', restricted: false, policy: 'open', listVersion: '0.1.1', reasonCode: null, message: null, sanctioned: false };
export const RESTRICTED_GEO: WireGeo = { country: 'BR', restricted: true, policy: 'exit_only', listVersion: '0.1.1', reasonCode: 'JURISDICTION', message: 'x', sanctioned: false };

export function status(state: 'running' | 'recovering' | 'halted' = 'running'): WireStatus {
  const ref = { height: 1, time: 1, hash: '0x', stateRoot: '0x', logHash: '0x' };
  return {
    name: 'n', version: '0', apiVersion: '0.1', mode: 'mock', unaudited: true,
    network: { eip712: { name: 'PosternPerps', version: '1', chainId: 7007701 }, collateralChainId: 700770, vault: '0x', collateralToken: '0x', collateralSymbol: 'bUSDC', collateralDecimals: 6 },
    time: 1, latestBlock: ref, latestFinalized: ref,
    sequencer: { state, blockIntervalMs: 200, pendingActions: 0 },
    committee: { n: 7, threshold: 5, online: 7 }, vaultMode: 'normal',
    l2: { finalizedBlock: 1, lastDepositBlock: 1, endpointsAgree: true },
  };
}

export const NOW = 1_794_878_400_000;
export function guard(account: string, over: Partial<Guard> = {}): Guard {
  return { account, eip712Problem: null, markets: markets(), tableProblems: new Map(), geo: OPEN_GEO, status: status(), now: NOW, ...over };
}
