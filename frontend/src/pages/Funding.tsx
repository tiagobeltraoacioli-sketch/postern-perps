/**
 * Funding: what the hourly payment between longs and shorts is, the rate now, the settled rates of the last week as
 * bars, and what this account paid or received. Every figure is the exchange's; arithmetic here is integer.
 */
import { useEffect, useMemo, useState } from 'react';
import { feed, go } from '../state/session';
import { useStore } from '../lib/store';
import { useActions } from '../state/actions';
import { api } from '../lib/api';
import { formatCountdown, formatDateTime, formatDuration, formatPx, formatRate, formatSize, formatUsd, DASH } from '../lib/format';
import { parseFixedLoose, RATE_DECIMALS, PX_DECIMALS } from '../lib/fixed';
import { fundingPayment } from '../lib/calc';
import { useNow } from '../trade/hooks';
import { clockSkewMs } from '../state/session';
import { AssetMark } from '../trade/AssetMark';
import type { WireFundingRate } from '../lib/types';

type Settled = { time: number; rate: bigint; markPx: bigint };
const HOURS_SHOWN = 168;
const SLOTS_MIN = 72;

/** Geometry of the bar chart: bars grow from a zero baseline; the scale is symmetric around it. Pure, tested. */
export function fundingBars(rates: readonly bigint[], height: number): { y: number; h: number; positive: boolean }[] {
  let peak = 0n;
  for (const r of rates) if ((r < 0n ? -r : r) > peak) peak = r < 0n ? -r : r;
  const half = height / 2;
  return rates.map((r) => {
    const magnitude = peak === 0n ? 0 : (Number(r < 0n ? -r : r) / Number(peak)) * (half - 2);
    // A non-zero rate is never drawn as nothing: at least one pixel.
    const h = r === 0n ? 0 : Math.max(1, magnitude);
    return { y: r >= 0n ? half - h : half, h, positive: r >= 0n };
  });
}

export function Funding({ symbol }: { symbol: string }) {
  const markets = useStore(feed.markets);
  const tickers = useStore(feed.tickers);
  const state = useStore(feed.account);
  const skew = useStore(clockSkewMs);
  const { account } = useActions();
  const now = useNow(1000) + (skew ?? 0);
  const market = markets.find((m) => m.symbol === symbol);
  const ticker = tickers.get(symbol);
  const [settled, setSettled] = useState<Settled[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [hover, setHover] = useState<number | null>(null);
  useEffect(() => {
    let live = true;
    setSettled(null);
    setFailed(false);
    api
      .get<{ funding: WireFundingRate[] }>(`/v1/markets/${symbol}/funding?limit=${HOURS_SHOWN}`)
      .then((r) => live && setSettled(r.funding.map((f) => ({ time: f.time, rate: parseFixedLoose(f.rate, RATE_DECIMALS), markPx: parseFixedLoose(f.markPrice, PX_DECIMALS) })).reverse()))
      .catch(() => live && setFailed(true));
    return () => {
      live = false;
    };
  }, [symbol]);
  const position = state?.account.positions.find((p) => p.symbol === symbol);
  const mine = useMemo(() => (state?.funding ?? []).filter((f) => f.symbol === symbol), [state?.funding, symbol]);
  const mineTotal = mine.reduce((t, f) => t + f.payment, 0n);
  const bars = useMemo(() => (settled ? fundingBars(settled.map((s) => s.rate), 160) : []), [settled]);
  const sum = settled?.reduce((t, s) => t + s.rate, 0n) ?? null;
  // What the position would pay or receive at the next funding time if the rate stayed (SPEC funding formula, calc.ts).
  const next = position && ticker ? fundingPayment(position.size, ticker.mark, ticker.fundingRate) : null;
  const shown = hover !== null && settled ? settled[hover] : null;
  const slots = Math.max(SLOTS_MIN, settled?.length ?? 0);
  const offset = slots - (settled?.length ?? 0);
  // The interval is the market's (one hour at launch; a dev stack may run a shorter one): say what it is.
  const every = market ? (market.fundingIntervalSec === 3600 ? '1 h' : formatDuration(market.fundingIntervalSec * 1000)) : '1 h';
  if (markets.length && !market)
    return (
      <div className="page-message">
        <p>{symbol} is not a market of this exchange.</p>
      </div>
    );
  return (
    <div className="page page-wide">
      <header className="page-head page-head-row">
        <div>
          <p className="eyebrow">Funding</p>
          <h1 className="display">Funding rates</h1>
          <p className="lead">
            Every funding interval (one hour at launch), longs and shorts of a market pay each other so its price stays near the oracle index. A positive rate: longs pay shorts. Negative: shorts pay longs. The exchange takes no part of it.
          </p>
        </div>
        <label className="field field-inline market-select">
          <span>Market</span>
          <select value={symbol} onChange={(e) => go({ page: 'funding', symbol: e.target.value })}>
            {markets.map((m) => (
              <option key={m.symbol} value={m.symbol}>
                {m.symbol}
              </option>
            ))}
          </select>
        </label>
      </header>
      <div className="cards cards-4">
        <div className="card">
          <span className="card-label">
            <AssetMark asset={market?.base ?? ''} size={18} /> {symbol} rate now (per {every})
          </span>
          <span className={`card-value num ${ticker ? (ticker.fundingRate >= 0n ? 'up' : 'down') : ''}`}>{ticker ? formatRate(ticker.fundingRate) : DASH}</span>
          <span className="fine">{ticker ? (ticker.fundingRate >= 0n ? 'Longs pay shorts' : 'Shorts pay longs') : ''} if the premium stays as it is.</span>
        </div>
        <div className="card">
          <span className="card-label">Next funding in</span>
          <span className="card-value num">{ticker ? formatCountdown(ticker.nextFundingTime - now) : DASH}</span>
          <span className="fine">{ticker ? `${formatDateTime(ticker.nextFundingTime)} UTC, on the exchange clock` : ''}</span>
        </div>
        <div className="card">
          <span className="card-label">Cap per interval ({every})</span>
          <span className="card-value num">{market ? `±${formatRate(market.maxFundingRate, 'never')}` : DASH}</span>
          <span className="fine">The rate can never exceed this, in either direction.</span>
        </div>
        <div className="card">
          <span className="card-label">{settled && settled.length > 0 ? `Sum of the last ${settled.length} settled` : 'Sum of settled rates'}</span>
          <span className={`card-value num ${sum !== null && sum !== 0n ? (sum > 0n ? 'up' : 'down') : ''}`}>{sum !== null && settled && settled.length > 0 ? formatRate(sum) : DASH}</span>
          <span className="fine">Positive: a long paid this share of its position value over that time. Negative: it received it.</span>
        </div>
      </div>
      <section className="panel" aria-label={`${symbol} settled funding rates`}>
        <header className="panel-head">
          <h2>Settled rates, one per {every}, oldest to newest</h2>
          <span className="fine num" aria-live="polite">
            {shown ? `${formatDateTime(shown.time)} UTC · ${formatRate(shown.rate)} · mark ${market ? formatPx(shown.markPx, market.priceDecimals) : ''}` : 'Point at a bar, or use the table below'}
          </span>
        </header>
        {failed ? (
          <p className="callout callout-warn chart-pad">The settled rates could not be read from the exchange API.</p>
        ) : !settled ? (
          <p className="fine chart-pad">Reading…</p>
        ) : settled.length === 0 ? (
          <p className="fine chart-pad">No funding has settled on this market yet.</p>
        ) : (
          <div className="funding-chart">
            <span className="funding-axis funding-axis-top">Longs pay</span>
            <span className="funding-axis funding-axis-bottom">Shorts pay</span>
            {/* A slot per interval; at least SLOTS_MIN slots wide so a short history is thin bars at the right (newest), not slabs. */}
            <svg viewBox={`0 0 ${slots * 6} 160`} preserveAspectRatio="none" role="img" aria-label={`${settled.length} settled funding rates of ${symbol}; the table below holds every value`} onMouseLeave={() => setHover(null)}>
              <line x1="0" x2={slots * 6} y1="80" y2="80" className="funding-zero" />
              {bars.map((b, i) => (
                <g key={settled[i].time} onMouseEnter={() => setHover(i)}>
                  <rect x={(offset + i) * 6} y="0" width="6" height="160" className={`funding-hit${hover === i ? ' on' : ''}`} />
                  <rect x={(offset + i) * 6 + 1} y={b.y} width="4" height={b.h} rx="1" className={b.positive ? 'funding-up' : 'funding-down'} />
                </g>
              ))}
            </svg>
          </div>
        )}
        {settled && settled.length > 0 && (
          <details className="funding-table">
            <summary>Every settled rate as a table</summary>
            <div className="table-wrap table-tall">
              <table>
                <thead>
                  <tr>
                    <th>Time (UTC)</th>
                    <th className="r">Rate</th>
                    <th className="r">Mark price</th>
                    <th>Direction</th>
                  </tr>
                </thead>
                <tbody>
                  {[...settled].reverse().map((s) => (
                    <tr key={s.time}>
                      <td className="num">{formatDateTime(s.time)}</td>
                      <td className={`r num ${s.rate >= 0n ? 'up' : 'down'}`}>{formatRate(s.rate)}</td>
                      <td className="r num">{market ? formatPx(s.markPx, market.priceDecimals) : DASH}</td>
                      <td>{s.rate > 0n ? 'Longs paid shorts' : s.rate < 0n ? 'Shorts paid longs' : 'No payment'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </details>
        )}
      </section>
      <section className="panel" aria-label="Your funding">
        <header className="panel-head">
          <h2>Your funding on {symbol}</h2>
          <button type="button" className="link-button" onClick={() => go({ page: 'history', tab: 'funding' })}>
            All payments
          </button>
        </header>
        {!account ? (
          <p className="fine chart-pad">Connect a wallet to see what you paid or received.</p>
        ) : (
          <dl className="table-totals">
            <div>
              <dt>Position</dt>
              <dd className="num">{position && market ? `${formatSize(position.size, market.sizeDecimals, 'always')} ${market.base}` : 'None'}</dd>
            </div>
            <div>
              <dt>At the next funding, if the rate stays (estimate)</dt>
              <dd className={`num ${next !== null ? (next >= 0n ? 'up' : 'down') : ''}`}>{next !== null ? `${formatUsd(next, { sign: 'always', dp: 4 })} USDC` : DASH}</dd>
            </div>
            <div>
              <dt>Net received (+) or paid (−), loaded history</dt>
              <dd className={`num ${mineTotal >= 0n ? 'up' : 'down'}`}>{mine.length ? `${formatUsd(mineTotal, { sign: 'always', dp: 4 })} USDC` : DASH}</dd>
            </div>
          </dl>
        )}
      </section>
    </div>
  );
}
