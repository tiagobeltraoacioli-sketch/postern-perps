/**
 * Live data: one WebSocket (docs/API.md section 9), frame-budgeted.
 *
 * Messages are parsed when they arrive and only QUEUED; once per animation frame the queue is applied and each
 * store that changed notifies once. A burst of 500 book deltas between two frames costs one render, and the cost
 * of a frame is bounded by what is on screen, not by the message rate. Sequence gaps in the book resubscribe.
 */
import { api } from './api';
import { config } from './config';
import * as convert from './convert';
import type { Level } from './calc';
import { Store } from './store';
import type * as T from './types';

export type Connection = {
  state: 'connecting' | 'open' | 'reconnecting' | 'offline';
  /** Round trip of the last ping, ms. */
  latencyMs: number | null;
  /** Wall-clock time of the last message. */
  lastMessageAt: number | null;
  attempts: number;
};
export type BookState = { symbol: string; bids: Level[]; asks: Level[]; seq: number; time: number; ready: boolean };
export type CandleState = { symbol: string; interval: string; candles: T.Candle[]; ready: boolean };
export type AccountState = {
  account: T.Account;
  orders: T.Order[]; // open and untriggered
  history: T.Order[]; // closed, newest first
  fills: T.Fill[];
  funding: T.FundingPayment[];
  deposits: T.Deposit[];
  withdrawals: T.Withdrawal[];
  agents: T.WireAgent[];
  ready: boolean;
};

const EMPTY_BOOK: BookState = { symbol: '', bids: [], asks: [], seq: 0, time: 0, ready: false };
const TRADES_KEPT = 200;
const HISTORY_KEPT = 2000;

type Frame = (cb: () => void) => void;
const nextFrame: Frame = (cb) => (typeof requestAnimationFrame === 'function' ? requestAnimationFrame(cb) : setTimeout(cb, 16));

/** Frame statistics of the feed itself (work per flush), read by the performance page and the harness. */
export const feedStats = { messages: 0, flushes: 0, maxBatch: 0, flushMs: [] as number[] };

export class Feed {
  readonly connection = new Store<Connection>({ state: 'connecting', latencyMs: null, lastMessageAt: null, attempts: 0 });
  readonly markets = new Store<T.Market[]>([]);
  readonly marketsError = new Store<string | null>(null);
  readonly tickers = new Store<ReadonlyMap<string, T.Ticker>>(new Map());
  readonly book = new Store<BookState>(EMPTY_BOOK);
  readonly trades = new Store<T.Trade[]>([]);
  readonly candles = new Store<CandleState>({ symbol: '', interval: '15m', candles: [], ready: false });
  readonly account = new Store<AccountState | null>(null);
  readonly status = new Store<T.WireStatus | null>(null);
  /** Order ids of fills and liquidations to announce (consumed by the toast layer). */
  readonly events = new Store<{ id: number; kind: 'fill' | 'liquidation' | 'order'; text: string }[]>([]);

  private socket: WebSocket | null = null;
  private queue: Record<string, unknown>[] = [];
  private scheduled = false;
  private closed = false;
  private retry: ReturnType<typeof setTimeout> | null = null;
  private ping: ReturnType<typeof setInterval> | null = null;
  private pingSentAt = 0;
  private subId = 0;
  private symbol = '';
  private interval = '15m';
  private address: string | null = null;
  private bids = new Map<string, bigint>();
  private asks = new Map<string, bigint>();
  private bookSeq = 0;
  private bookReady = false;
  private eventId = 0;

  constructor(private wsUrl: string = config.wsUrl) {}

  start() {
    void this.loadMarkets();
    this.open();
    if (typeof window !== 'undefined') {
      window.addEventListener('online', this.onOnline);
      window.addEventListener('offline', this.onOffline);
    }
  }
  stop() {
    this.closed = true;
    if (this.retry) clearTimeout(this.retry);
    if (this.ping) clearInterval(this.ping);
    this.socket?.close();
    if (typeof window !== 'undefined') {
      window.removeEventListener('online', this.onOnline);
      window.removeEventListener('offline', this.onOffline);
    }
  }
  private onOnline = () => this.open();
  private onOffline = () => this.connection.update((c) => ({ ...c, state: 'offline' }));

  async loadMarkets() {
    try {
      const [m, t, s] = await Promise.all([
        api.get<{ markets: T.WireMarket[] }>('/v1/markets'),
        api.get<{ tickers: T.WireTicker[] }>('/v1/tickers'),
        api.get<T.WireStatus>('/v1/status'),
      ]);
      this.markets.set(m.markets.map(convert.market));
      this.tickers.set(new Map(t.tickers.map((x) => [x.symbol, convert.ticker(x)])));
      this.status.set(s);
      this.marketsError.set(null);
    } catch {
      this.marketsError.set('Markets could not be read from the exchange API.');
    }
  }

  /** Selects the market whose book, trades and candles are streamed. */
  select(symbol: string, interval = this.interval) {
    if (symbol === this.symbol && interval === this.interval) return;
    const marketChanged = symbol !== this.symbol;
    const previous = { symbol: this.symbol, interval: this.interval };
    this.symbol = symbol;
    this.interval = interval;
    if (marketChanged) {
      this.resetBook();
      this.trades.set([]);
    }
    this.candles.set({ symbol, interval, candles: [], ready: false });
    if (this.socket?.readyState === WebSocket.OPEN) {
      if (previous.symbol) {
        if (marketChanged) for (const channel of ['book', 'trades']) this.send({ op: 'unsubscribe', channel, symbol: previous.symbol });
        this.send({ op: 'unsubscribe', channel: 'candles', symbol: previous.symbol, interval: previous.interval });
      }
      this.subscribeMarket(marketChanged);
    }
  }
  /** Follows an account (its main address), or none. */
  follow(address: string | null) {
    const next = address?.toLowerCase() ?? null;
    if (next === this.address) return;
    if (this.address && this.socket?.readyState === WebSocket.OPEN) this.send({ op: 'unsubscribe', channel: 'account', account: this.address });
    this.address = next;
    this.account.set(null);
    if (next && this.socket?.readyState === WebSocket.OPEN) this.send({ op: 'subscribe', channel: 'account', account: next });
    if (next) void this.loadHistory(next);
  }

  private async loadHistory(address: string) {
    try {
      const [orders, fills, funding, deposits, withdrawals] = await Promise.all([
        api.get<{ orders: T.WireOrder[] }>(`/v1/accounts/${address}/orders?status=all&limit=500`),
        api.get<{ fills: T.WireFill[] }>(`/v1/accounts/${address}/fills?limit=500`),
        api.get<{ payments: T.WireFundingPayment[] }>(`/v1/accounts/${address}/funding?limit=500`),
        api.get<{ deposits: T.WireDeposit[] }>(`/v1/accounts/${address}/deposits`),
        api.get<{ withdrawals: T.WireWithdrawal[] }>(`/v1/accounts/${address}/withdrawals`),
      ]);
      if (address !== this.address) return;
      this.pendingHistory = {
        history: orders.orders.map(convert.order).filter((o) => o.status !== 'open' && o.status !== 'untriggered'),
        fills: fills.fills.map(convert.fill),
        funding: funding.payments.map(convert.fundingPayment),
        deposits: deposits.deposits.map(convert.deposit),
        withdrawals: withdrawals.withdrawals.map(convert.withdrawal),
      };
      const current = this.account.get();
      if (current) this.account.set({ ...current, ...this.pendingHistory });
    } catch {
      /* history stays empty; the live channel still works */
    }
  }
  private pendingHistory: Pick<AccountState, 'history' | 'fills' | 'funding' | 'deposits' | 'withdrawals'> | null = null;

  private open() {
    if (this.closed || (this.socket && this.socket.readyState <= WebSocket.OPEN)) return;
    let socket: WebSocket;
    try {
      socket = new WebSocket(this.wsUrl);
    } catch {
      this.scheduleRetry();
      return;
    }
    this.socket = socket;
    socket.onopen = () => {
      this.connection.update((c) => ({ ...c, state: 'open', attempts: 0, lastMessageAt: Date.now() }));
      this.send({ op: 'subscribe', channel: 'ticker', symbol: '*' });
      this.send({ op: 'subscribe', channel: 'status' });
      if (this.symbol) this.subscribeMarket(true);
      if (this.address) this.send({ op: 'subscribe', channel: 'account', account: this.address });
      if (this.markets.get().length === 0) void this.loadMarkets();
      if (this.ping) clearInterval(this.ping);
      this.ping = setInterval(() => {
        const c = this.connection.get();
        // No frame for 20 s: the socket is dead even if the browser has not noticed.
        if (c.lastMessageAt && Date.now() - c.lastMessageAt > 20_000) socket.close();
        else {
          this.pingSentAt = performance.now();
          this.send({ op: 'ping' });
        }
      }, 5_000);
    };
    socket.onmessage = (event) => {
      let message: Record<string, unknown>;
      try {
        message = JSON.parse(String(event.data));
      } catch {
        return;
      }
      feedStats.messages++;
      this.queue.push(message);
      if (!this.scheduled) {
        this.scheduled = true;
        nextFrame(this.flush);
      }
    };
    socket.onclose = () => {
      if (this.socket !== socket) return;
      this.socket = null;
      if (this.ping) clearInterval(this.ping);
      this.bookReady = false;
      this.book.update((b) => ({ ...b, ready: false }));
      this.scheduleRetry();
    };
    socket.onerror = () => socket.close();
  }
  private scheduleRetry() {
    if (this.closed) return;
    const attempts = this.connection.get().attempts + 1;
    this.connection.update((c) => ({ ...c, state: typeof navigator !== 'undefined' && navigator.onLine === false ? 'offline' : 'reconnecting', attempts, latencyMs: null }));
    if (this.retry) clearTimeout(this.retry);
    this.retry = setTimeout(() => this.open(), Math.min(15_000, 500 * 2 ** Math.min(attempts, 5)));
  }
  private send(message: Record<string, unknown>) {
    if (this.socket?.readyState === WebSocket.OPEN) this.socket.send(JSON.stringify({ id: ++this.subId, ...message }));
  }
  private subscribeMarket(all: boolean) {
    if (all) {
      this.send({ op: 'subscribe', channel: 'book', symbol: this.symbol, depth: 200 });
      this.send({ op: 'subscribe', channel: 'trades', symbol: this.symbol });
    }
    this.send({ op: 'subscribe', channel: 'candles', symbol: this.symbol, interval: this.interval });
  }
  private resetBook() {
    this.bids.clear();
    this.asks.clear();
    this.bookSeq = 0;
    this.bookReady = false;
    this.book.set({ ...EMPTY_BOOK, symbol: this.symbol });
  }

  /** Applies everything queued since the last frame; each store notifies at most once. */
  private flush = () => {
    this.scheduled = false;
    const batch = this.queue;
    this.queue = [];
    const started = performance.now();
    let bookDirty = false;
    let bookTime = 0;
    let tickers: Map<string, T.Ticker> | null = null;
    let trades: T.Trade[] | null = null;
    let candles: CandleState | null = null;
    let account: AccountState | null | undefined;
    let status: T.WireStatus | null = null;
    let latency: number | null = null;
    for (const m of batch) {
      if (m.op === 'pong') {
        latency = Math.round(performance.now() - this.pingSentAt);
        continue;
      }
      const data = m.data as never;
      switch (m.channel) {
        case 'ticker': {
          tickers ??= new Map(this.tickers.get());
          const t = convert.ticker(data as T.WireTicker);
          tickers.set(t.symbol, t);
          break;
        }
        case 'book': {
          if (m.symbol !== this.symbol) break;
          const b = data as T.WireBook;
          if (m.type === 'snapshot') {
            this.bids.clear();
            this.asks.clear();
            this.bookReady = true;
          } else if (!this.bookReady) break;
          else if (b.prevSeq !== this.bookSeq) {
            // A message was missed: ask for a fresh snapshot and ignore deltas until it comes.
            this.bookReady = false;
            this.send({ op: 'unsubscribe', channel: 'book', symbol: this.symbol });
            this.send({ op: 'subscribe', channel: 'book', symbol: this.symbol, depth: 200 });
            break;
          }
          for (const [side, levels] of [[this.bids, b.bids], [this.asks, b.asks]] as const)
            for (const [price, size] of levels) {
              const l = convert.level([price, size]);
              if (l.size === 0n) side.delete(price);
              else side.set(price, l.size);
            }
          this.bookSeq = b.seq;
          bookTime = b.time;
          bookDirty = true;
          break;
        }
        case 'trades': {
          if (m.symbol !== this.symbol) break;
          const incoming = (data as { trades: T.WireTrade[] }).trades.map(convert.trade);
          if (m.type === 'snapshot') trades = incoming.slice(0, TRADES_KEPT);
          else {
            trades ??= this.trades.get();
            // Updates are oldest first; the list is newest first.
            trades = [...incoming.reverse(), ...trades].slice(0, TRADES_KEPT);
          }
          break;
        }
        case 'candles': {
          if (m.symbol !== this.symbol || m.interval !== this.interval) break;
          if (m.type === 'snapshot') candles = { symbol: this.symbol, interval: this.interval, candles: (data as { candles: T.WireCandle[] }).candles.map(convert.candle), ready: true };
          else {
            candles ??= this.candles.get();
            if (!candles.ready) break;
            const c = convert.candle(data as T.WireCandle);
            const list = candles.candles;
            const last = list[list.length - 1];
            candles = { ...candles, candles: last && last.t === c.t ? [...list.slice(0, -1), c] : last && c.t < last.t ? list : [...list, c].slice(-1500) };
          }
          break;
        }
        case 'account': {
          if (String(m.account ?? '').toLowerCase() !== this.address) break;
          if (m.type === 'snapshot') {
            const s = data as { account: T.WireAccount; orders: T.WireOrder[]; agents: T.WireAgent[] };
            const base = this.pendingHistory ?? { history: [], fills: [], funding: [], deposits: [], withdrawals: [] };
            account = { account: convert.account(s.account), orders: s.orders.map(convert.order), agents: s.agents ?? [], ...base, ready: true };
          } else {
            const current = account === undefined ? this.account.get() : account;
            if (current) account = this.applyAccountUpdate(current, data as T.WireAccountUpdate & { agents?: T.WireAgent[] });
          }
          break;
        }
        case 'status':
          if (this.status.get()) status = { ...(status ?? this.status.get()!), ...(data as Partial<T.WireStatus>) };
          break;
      }
    }
    if (tickers) this.tickers.set(tickers);
    if (bookDirty) {
      const sorted = (side: Map<string, bigint>, descending: boolean) => {
        const levels: Level[] = [];
        for (const [price, size] of side) levels.push({ px: convert.level([price, '0']).px, size });
        levels.sort((a, b) => (a.px < b.px ? -1 : a.px > b.px ? 1 : 0));
        if (descending) levels.reverse();
        return levels;
      };
      this.book.set({ symbol: this.symbol, bids: sorted(this.bids, true), asks: sorted(this.asks, false), seq: this.bookSeq, time: bookTime, ready: true });
    }
    if (trades) this.trades.set(trades);
    if (candles) this.candles.set(candles);
    if (account !== undefined) this.account.set(account);
    if (status) this.status.set(status);
    this.connection.update((c) => ({ ...c, lastMessageAt: Date.now(), latencyMs: latency ?? c.latencyMs }));
    feedStats.flushes++;
    feedStats.maxBatch = Math.max(feedStats.maxBatch, batch.length);
    if (feedStats.flushMs.length < 20_000) feedStats.flushMs.push(performance.now() - started);
  };

  private applyAccountUpdate(current: AccountState, u: T.WireAccountUpdate & { agents?: T.WireAgent[] }): AccountState {
    let { orders, history, fills, funding, deposits, withdrawals } = current;
    if (u.orders) {
      const changed = u.orders.map(convert.order);
      const ids = new Set(changed.map((o) => o.id));
      orders = [...changed.filter((o) => o.status === 'open' || o.status === 'untriggered'), ...orders.filter((o) => !ids.has(o.id))];
      const closed = changed.filter((o) => o.status !== 'open' && o.status !== 'untriggered');
      if (closed.length) history = [...closed, ...history.filter((o) => !ids.has(o.id))].slice(0, HISTORY_KEPT);
    }
    if (u.fills) {
      const incoming = u.fills.map(convert.fill);
      fills = [...incoming.reverse(), ...fills].slice(0, HISTORY_KEPT);
      for (const f of incoming)
        this.announce(f.liquidation ? 'liquidation' : 'fill', f.liquidation ? `Liquidation: your ${f.symbol} position was reduced by the exchange.` : `Filled: ${f.buy ? 'bought' : 'sold'} ${f.symbol}.`);
    }
    if (u.funding) funding = [...u.funding.map(convert.fundingPayment).reverse(), ...funding].slice(0, HISTORY_KEPT);
    const merge = <X extends { id: string }>(list: X[], incoming: X[]) => {
      const ids = new Set(incoming.map((x) => x.id));
      return [...incoming, ...list.filter((x) => !ids.has(x.id))];
    };
    if (u.deposits) deposits = merge(deposits, u.deposits.map(convert.deposit));
    if (u.withdrawals) withdrawals = merge(withdrawals, u.withdrawals.map(convert.withdrawal));
    return { ...current, account: u.account ? convert.account(u.account) : current.account, orders, history, fills, funding, deposits, withdrawals, agents: u.agents ?? current.agents };
  }
  private announce(kind: 'fill' | 'liquidation' | 'order', text: string) {
    this.events.update((e) => [...e.slice(-4), { id: ++this.eventId, kind, text }]);
  }
}
