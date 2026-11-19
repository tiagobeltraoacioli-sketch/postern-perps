/** Markets: every market the exchange publishes, as the exchange reports it. Click a row to trade it. */
import { useEffect, useMemo, useState } from 'react';
import { Search } from 'lucide-react';
import { feed, go } from '../state/session';
import { useStore } from '../lib/store';
import { formatBps, formatCompactUsd, formatPpm, formatPx, formatRate, formatSize, DASH } from '../lib/format';
import { notional } from '../lib/calc';
import { AssetMark } from '../trade/AssetMark';
import { signingProblem } from '../lib/pins';
import type { Market, Ticker, WireCandle } from '../lib/types';
import { api } from '../lib/api';
import { persisted } from '../lib/store';

type SortKey = 'symbol' | 'change' | 'volume' | 'oi' | 'funding';
const STATUS_WORDS: Record<Market['status'], string> = { active: 'Trading', halted: 'Halted', closeOnly: 'Close only', settled: 'Settled' };

export function sortMarkets(markets: Market[], tickers: ReadonlyMap<string, Ticker>, key: SortKey, descending: boolean): Market[] {
  const value = (m: Market): bigint | null => {
    const t = tickers.get(m.symbol);
    if (!t) return null;
    if (key === 'change') return t.changePpm;
    if (key === 'volume') return t.volume24h;
    if (key === 'oi') return notional(t.openInterest, t.mark);
    return t.fundingRate;
  };
  const out = [...markets];
  if (key === 'symbol') out.sort((a, b) => a.marketId - b.marketId);
  else
    out.sort((a, b) => {
      const va = value(a);
      const vb = value(b);
      // A market without a ticker sorts last whatever the direction: an unknown is never ranked as zero.
      if (va === null || vb === null) return va === vb ? a.marketId - b.marketId : va === null ? 1 : -1;
      return va === vb ? a.marketId - b.marketId : (va < vb ? -1 : 1) * (descending ? -1 : 1);
    });
  return out;
}

/** Geometry of a sparkline: closes mapped into a w x h box (y down), as an SVG path. Null under two points. Pure. */
export function sparkPath(closes: readonly number[], w: number, h: number): string | null {
  const pts = closes.filter((c) => Number.isFinite(c));
  if (pts.length < 2) return null;
  const lo = Math.min(...pts);
  const hi = Math.max(...pts);
  const span = hi - lo;
  const y = (c: number) => (span === 0 ? h / 2 : h - 1 - ((c - lo) / span) * (h - 2));
  return pts.map((c, i) => `${i ? 'L' : 'M'}${((i / (pts.length - 1)) * w).toFixed(2)} ${y(c).toFixed(2)}`).join('');
}

/** The last two days of hourly closes per market (drawing only: no figure is derived from them). */
function useSparks(symbols: string): ReadonlyMap<string, number[] | 'none'> {
  const [sparks, setSparks] = useState<ReadonlyMap<string, number[] | 'none'>>(new Map());
  useEffect(() => {
    let live = true;
    const list = symbols ? symbols.split(',') : [];
    void (async () => {
      // One market at a time: a page of thirty markets must not open thirty requests at once.
      for (const symbol of list) {
        let closes: number[] | 'none' = 'none';
        try {
          const r = await api.get<{ candles: WireCandle[] }>(`/v1/markets/${symbol}/candles?interval=1h&limit=48`);
          const c = r.candles.map((x) => Number(x.c)).filter((x) => Number.isFinite(x) && x > 0);
          if (c.length >= 2) closes = c;
        } catch {
          closes = 'none';
        }
        if (!live) return;
        setSparks((m) => new Map(m).set(symbol, closes));
      }
    })();
    return () => {
      live = false;
    };
  }, [symbols]);
  return sparks;
}

function Spark({ closes, label }: { closes: number[] | 'none' | undefined; label: string }) {
  if (closes === undefined) return <span className="skeleton spark-empty" aria-hidden="true" />;
  const d = closes === 'none' ? null : sparkPath(closes, 120, 32);
  if (!d || closes === 'none') return <span className="spark-empty">No trades yet</span>;
  const tone = closes[closes.length - 1] > closes[0] ? 'up' : closes[closes.length - 1] < closes[0] ? 'down' : 'flat';
  return (
    <svg className={`spark spark-${tone}`} viewBox="0 0 120 32" preserveAspectRatio="none" role="img" aria-label={label}>
      <path d={d} />
    </svg>
  );
}

type View = 'cards' | 'table';
const marketsView = persisted<View | 'auto'>('postern.perps.markets.view.v1', 'auto', (v): v is View | 'auto' => v === 'cards' || v === 'table' || v === 'auto');

export function Markets() {
  const markets = useStore(feed.markets);
  const tickers = useStore(feed.tickers);
  const error = useStore(feed.marketsError);
  const [query, setQuery] = useState('');
  const chosen = useStore(marketsView);
  // A handful of markets reads best as cards; a long list as a table. The switch remembers a choice.
  const view: View = chosen === 'auto' ? (markets.length <= 8 ? 'cards' : 'table') : chosen;
  const sparks = useSparks(markets.map((m) => m.symbol).join(','));
  const [sort, setSort] = useState<{ key: SortKey; descending: boolean }>({ key: 'symbol', descending: false });
  const rows = useMemo(() => {
    const q = query.trim().toUpperCase();
    return sortMarkets(q ? markets.filter((m) => m.symbol.includes(q) || m.base.includes(q)) : markets, tickers, sort.key, sort.descending);
  }, [markets, tickers, query, sort]);
  const totals = useMemo(() => {
    let volume = 0n;
    let oi = 0n;
    for (const m of markets) {
      const t = tickers.get(m.symbol);
      if (!t) continue;
      volume += t.volume24h;
      oi += notional(t.openInterest, t.mark);
    }
    return { volume, oi };
  }, [markets, tickers]);
  const head = (key: SortKey, label: string) => (
    <th className="r" aria-sort={sort.key === key ? (sort.descending ? 'descending' : 'ascending') : 'none'}>
      <button type="button" className="th-sort" onClick={() => setSort((s) => ({ key, descending: s.key === key ? !s.descending : true }))}>
        {label}
        <span aria-hidden="true">{sort.key === key ? (sort.descending ? ' ↓' : ' ↑') : ''}</span>
      </button>
    </th>
  );
  return (
    <div className="page page-wide">
      <header className="page-head page-head-row">
        <div>
          <p className="eyebrow">Markets</p>
          <h1 className="display">Perpetual markets</h1>
          <p className="lead">USD-quoted perpetuals, margined and settled in bUSDC. Every figure is the exchange's own, read live.</p>
        </div>
        <dl className="head-stats">
          <div>
            <dt>24 h volume</dt>
            <dd className="num">${formatCompactUsd(totals.volume)}</dd>
          </div>
          <div>
            <dt>Open interest</dt>
            <dd className="num">${formatCompactUsd(totals.oi)}</dd>
          </div>
          <div>
            <dt>Markets</dt>
            <dd className="num">{markets.length || DASH}</dd>
          </div>
        </dl>
      </header>
      {error && <p className="callout callout-danger">{error}</p>}
      <section className="panel" aria-label="Markets">
        <header className="panel-head markets-head">
          <label className="search">
            <Search size={16} aria-hidden="true" />
            <span className="sr-only">Search markets</span>
            <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search" spellCheck={false} />
          </label>
          <span className="fine">Funding is the hourly rate that would be charged at the next funding time. Positive: longs pay shorts.</span>
          <div className="segmented view-switch" role="group" aria-label="Show markets as">
            {(['cards', 'table'] as View[]).map((v) => (
              <button key={v} type="button" aria-pressed={view === v} onClick={() => marketsView.set(v)}>
                {v === 'cards' ? 'Cards' : 'Table'}
              </button>
            ))}
          </div>
        </header>
        {view === 'cards' && rows.length > 0 && (
          <div className="market-cards chart-pad">
            {rows.map((m) => {
              const t = tickers.get(m.symbol);
              return (
                <button key={m.symbol} type="button" className="market-card" onClick={() => go({ page: 'trade', symbol: m.symbol })} aria-label={`Trade ${m.symbol}`}>
                  <span className="market-card-head">
                    <AssetMark asset={m.base} size={28} />
                    <span className="market-card-name">
                      <strong>{m.symbol}</strong>
                      <span className="sub">Perp · {m.maxLeverage}x</span>
                    </span>
                    <span className={`badge ${m.status === 'active' ? 'status-credited' : m.status === 'halted' ? 'badge-danger' : 'badge-warn'}`}>{STATUS_WORDS[m.status]}</span>
                  </span>
                  <span className="market-card-px">
                    <span className="num">{t ? formatPx(t.mark, m.priceDecimals) : DASH}</span>
                    <span className={`num ${t ? (t.changePpm > 0n ? 'up' : t.changePpm < 0n ? 'down' : '') : ''}`}>{t ? formatPpm(t.changePpm, 2, 'always') : DASH}</span>
                  </span>
                  <Spark closes={sparks.get(m.symbol)} label={`${m.symbol}: hourly closes of the last two days`} />
                  <dl>
                    <div>
                      <dt>24 h volume</dt>
                      <dd className="num">{t ? `$${formatCompactUsd(t.volume24h)}` : DASH}</dd>
                    </div>
                    <div>
                      <dt>Open interest</dt>
                      <dd className="num">{t ? `$${formatCompactUsd(notional(t.openInterest, t.mark))}` : DASH}</dd>
                    </div>
                    <div>
                      <dt>Funding (1 h)</dt>
                      <dd className={`num ${t ? (t.fundingRate >= 0n ? 'up' : 'down') : ''}`}>{t ? formatRate(t.fundingRate) : DASH}</dd>
                    </div>
                  </dl>
                </button>
              );
            })}
          </div>
        )}
        {view === 'cards' && rows.length === 0 && <p className="fine chart-pad">{markets.length ? 'No market matches.' : error ? 'Markets could not be read.' : 'Reading markets…'}</p>}
        <div className="table-wrap" hidden={view !== 'table'}>
          <table className="markets-table">
            <thead>
              <tr>
                <th aria-sort={sort.key === 'symbol' ? 'ascending' : 'none'}>
                  <button type="button" className="th-sort" onClick={() => setSort({ key: 'symbol', descending: false })}>
                    Market
                  </button>
                </th>
                <th className="r">Mark price</th>
                <th className="r">Oracle index</th>
                {head('change', '24 h change')}
                {head('volume', '24 h volume')}
                {head('oi', 'Open interest')}
                {head('funding', 'Funding (1 h)')}
                <th>Last 48 h</th>
                <th className="r">Max leverage</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((m) => {
                const t = tickers.get(m.symbol);
                const pin = signingProblem(m);
                return (
                  <tr key={m.symbol} className="row-link" onClick={() => go({ page: 'trade', symbol: m.symbol })}>
                    <td>
                      <button type="button" className="market-cell" onClick={(e) => (e.stopPropagation(), go({ page: 'trade', symbol: m.symbol }))} aria-label={`Trade ${m.symbol}`}>
                        <AssetMark asset={m.base} size={26} />
                        <strong>{m.symbol}</strong>
                        <span className="sub">Perp</span>
                      </button>
                    </td>
                    <td className="r num strong">{t ? formatPx(t.mark, m.priceDecimals) : DASH}</td>
                    <td className="r num">{t ? formatPx(t.index, m.priceDecimals) : DASH}</td>
                    <td className={`r num ${t ? (t.changePpm >= 0n ? 'up' : 'down') : ''}`}>{t ? formatPpm(t.changePpm, 2, 'always') : DASH}</td>
                    <td className="r num">{t ? `$${formatCompactUsd(t.volume24h)}` : DASH}</td>
                    <td className="r num">{t ? `$${formatCompactUsd(notional(t.openInterest, t.mark))}` : DASH}</td>
                    <td className={`r num ${t ? (t.fundingRate >= 0n ? 'up' : 'down') : ''}`}>{t ? formatRate(t.fundingRate) : DASH}</td>
                    <td>
                      <Spark closes={sparks.get(m.symbol)} label={`${m.symbol}: hourly closes of the last two days`} />
                    </td>
                    <td className="r num">{m.maxLeverage}x</td>
                    <td>
                      <span className={`badge ${m.status === 'active' ? 'status-credited' : m.status === 'halted' ? 'badge-danger' : 'badge-warn'}`}>{STATUS_WORDS[m.status]}</span>
                      {pin && <span className="badge badge-danger" title={pin}>signing refused</span>}
                    </td>
                  </tr>
                );
              })}
              {rows.length === 0 && (
                <tr>
                  <td colSpan={10} className="empty">
                    {markets.length ? 'No market matches.' : error ? 'Markets could not be read.' : 'Reading markets…'}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </section>
      <section className="panel" aria-label="Market parameters">
        <header className="panel-head">
          <h2>Contract parameters, as the exchange publishes them</h2>
          <span className="fine">Requirements grow with position size; the first tier is shown.</span>
        </header>
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Market</th>
                <th className="r">Tick</th>
                <th className="r">Size step</th>
                <th className="r">Smallest order</th>
                <th className="r">Largest order</th>
                <th className="r">Initial margin</th>
                <th className="r">Maintenance margin</th>
                <th className="r">Taker / maker</th>
                <th className="r">Liquidation fee</th>
                <th className="r">Funding cap (1 h)</th>
              </tr>
            </thead>
            <tbody>
              {markets.map((m) => (
                <tr key={m.symbol}>
                  <td>
                    <strong>{m.symbol}</strong>
                  </td>
                  <td className="r num">{formatPx(m.tick, m.priceDecimals)}</td>
                  <td className="r num">{formatSize(m.step, m.sizeDecimals)}</td>
                  <td className="r num">
                    {formatSize(m.minSize, m.sizeDecimals)} {m.base}
                  </td>
                  <td className="r num">
                    {formatSize(m.maxSize, m.sizeDecimals)} {m.base}
                  </td>
                  <td className="r num">{m.tiers[0] ? formatPpm(m.tiers[0].imfPpm, 2) : DASH}</td>
                  <td className="r num">{m.tiers[0] ? formatPpm(m.tiers[0].mmfPpm, 2) : DASH}</td>
                  <td className="r num">
                    {formatBps(m.takerPpm)} / {formatBps(m.makerPpm)}
                  </td>
                  <td className="r num">{formatPpm(m.liquidationFeePpm, 2)}</td>
                  <td className="r num">±{formatRate(m.maxFundingRate, 'never')}</td>
                </tr>
              ))}
              {markets.length === 0 && (
                <tr>
                  <td colSpan={10} className="empty">
                    {error ? 'Markets could not be read.' : 'Reading markets…'}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  );
}
