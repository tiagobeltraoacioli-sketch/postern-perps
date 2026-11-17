import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { Feed, feedStats } from '../lib/feed';
import { aggregate } from '../trade/BookAndTrades';
import { chartInputs } from '../trade/ChartPanel';
import { market } from './fixtures';
import { PX, SZ } from '../lib/fixed';
import type { AccountState } from '../lib/feed';

class FakeSocket {
  static last: FakeSocket | null = null;
  static OPEN = 1;
  readyState = 0;
  sent: Record<string, unknown>[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(readonly url: string) {
    FakeSocket.last = this;
  }
  send(text: string) {
    this.sent.push(JSON.parse(text));
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
  open() {
    this.readyState = 1;
    this.onopen?.();
  }
  push(message: unknown) {
    this.onmessage?.({ data: JSON.stringify(message) });
  }
}

describe('feed', () => {
  beforeEach(() => {
    vi.stubGlobal('WebSocket', FakeSocket);
    vi.stubGlobal('requestAnimationFrame', (cb: () => void) => setTimeout(cb, 0));
    vi.stubGlobal('fetch', vi.fn(async () => new Response('{}', { status: 500 })));
  });
  afterEach(() => vi.unstubAllGlobals());

  const flush = () => new Promise((r) => setTimeout(r, 5));
  const snapshot = (seq: number) => ({ channel: 'book', symbol: 'BTC-USD', type: 'snapshot', data: { symbol: 'BTC-USD', seq, height: 1, time: 1, bids: [['67000.0', '1.0']], asks: [['67001.0', '2.0']] } });

  it('subscription acknowledgements do not break the batch (regression: an ack with a channel and no data threw)', async () => {
    const f = new Feed('ws://x');
    f.select('BTC-USD');
    f.start();
    FakeSocket.last!.open();
    expect(FakeSocket.last!.sent.some((m) => m.channel === 'book' && m.op === 'subscribe')).toBe(true);
    FakeSocket.last!.push({ op: 'subscribed', id: 1, channel: 'ticker', symbol: '*' });
    FakeSocket.last!.push(snapshot(10));
    await flush();
    expect(f.book.get().ready).toBe(true);
    expect(f.book.get().bids[0]).toEqual({ px: 67_000n * PX, size: SZ });
    f.stop();
  });

  it('a malformed message is counted and skipped; the rest of the batch applies', async () => {
    const f = new Feed('ws://x');
    f.select('BTC-USD');
    f.start();
    FakeSocket.last!.open();
    const before = feedStats.malformed;
    FakeSocket.last!.push({ channel: 'trades', symbol: 'BTC-USD', type: 'update', data: { trades: 'nonsense' } });
    FakeSocket.last!.push(snapshot(10));
    await flush();
    expect(feedStats.malformed).toBe(before + 1);
    expect(f.book.get().ready).toBe(true);
    f.stop();
  });

  it('a sequence gap stops applying deltas and asks for a fresh snapshot', async () => {
    const f = new Feed('ws://x');
    f.select('BTC-USD');
    f.start();
    const s = FakeSocket.last!;
    s.open();
    s.push(snapshot(10));
    await flush();
    const sentBefore = s.sent.length;
    s.push({ channel: 'book', symbol: 'BTC-USD', type: 'update', data: { symbol: 'BTC-USD', seq: 12, prevSeq: 11, height: 2, time: 2, bids: [['66999.0', '5.0']], asks: [] } });
    await flush();
    expect(f.book.get().bids.some((l) => l.px === 66_999n * PX)).toBe(false);
    expect(s.sent.slice(sentBefore).map((m) => `${m.op}:${m.channel}`)).toEqual(['unsubscribe:book', 'subscribe:book']);
    f.stop();
  });
});

describe('book aggregation', () => {
  it('bids round down and asks round up to the group, sizes add', () => {
    const g = 10n * PX;
    expect(aggregate([{ px: 67_005n * PX, size: SZ }, { px: 67_001n * PX, size: SZ }], g, true)).toEqual([{ px: 67_000n * PX, size: 2n * SZ }]);
    expect(aggregate([{ px: 67_001n * PX, size: SZ }, { px: 67_009n * PX, size: SZ }], g, false)).toEqual([{ px: 67_010n * PX, size: 2n * SZ }]);
  });
});

describe('chart inputs', () => {
  it('passes exact texts and the exchange-reported liquidation price; triggers at their trigger price', () => {
    const btc = market('BTC-USD');
    const state = {
      account: { positions: [{ symbol: 'BTC-USD', size: SZ / 2n, entryPx: 66_800n * PX, markPx: 0n, notional: 0n, unrealised: 225_600_000n, realised: 0n, liquidationPx: 60_000n * PX, isolated: false, leverage: 10, isolatedMargin: 0n, initialMargin: 0n, maintenanceMargin: 0n, funding: 0n }] },
      orders: [{ id: '7', symbol: 'BTC-USD', buy: false, type: 'stopMarket', tif: 'IOC', px: 63_000n * PX, size: SZ / 2n, filled: 0n, avgPx: null, reduceOnly: true, triggerPx: 63_900n * PX, triggerCondition: 'markBelow', status: 'untriggered', createdAt: 0, updatedAt: 0, rejectReason: null }],
      fills: [],
    } as unknown as AccountState;
    const { levels } = chartInputs(btc, state);
    expect(levels.map((l) => [l.kind, l.priceText, l.label])).toEqual([
      ['position', '66,800.0', 'Long 0.5000 BTC'],
      ['liquidation', '60,000.0', 'Liquidation'],
      ['trigger', '63,900.0', 'Stop 0.5000'],
    ]);
    expect(levels[0].detail).toBe('+225.60 USDC');
  });
});
