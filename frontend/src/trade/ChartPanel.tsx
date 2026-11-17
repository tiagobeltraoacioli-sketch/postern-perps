/**
 * The chart slot. The chart itself is agent 396's component (`frontend/src/chart/`, interface v0.1.0); this file only
 * turns the app's exact values into its props (`chartInputs`, tested) and renders `PriceChart` when the chart
 * module exports it. Until then it renders a plain reading of the last candle and says the chart is pending, so
 * the trading screen is complete without inventing a drawing.
 */
import { type ComponentType } from 'react';
import * as chart from '../chart';
import type { ChartInterval, ChartLevel, ChartFill, ChartNotice, PriceChartProps } from '../chart';
import { feed } from '../state/session';
import { useStore, persisted } from '../lib/store';
import { formatPx, formatSize, formatUsd } from '../lib/format';
import { pxNumber } from '../lib/convert';
import type { AccountState } from '../lib/feed';
import type { Market, Ticker } from '../lib/types';
import { pricePick } from './hooks';
import { useTheme } from '../ui/theme';
import { toDecimalString, PX_DECIMALS } from '../lib/fixed';

const INTERVALS: ChartInterval[] = ['1m', '5m', '15m', '1h', '4h', '1d'];
export const chartInterval = persisted<ChartInterval>('postern.perps.interval.v1', '15m', (v): v is ChartInterval => INTERVALS.includes(v as ChartInterval));

/** The user's levels and fills in this market, as the chart's interface wants them (exact texts from the app). */
export function chartInputs(market: Market, account: AccountState | null): { levels: ChartLevel[]; fills: ChartFill[] } {
  const dp = market.priceDecimals;
  const sd = market.sizeDecimals;
  const levels: ChartLevel[] = [];
  const fills: ChartFill[] = [];
  if (!account) return { levels, fills };
  for (const p of account.account.positions.filter((x) => x.symbol === market.symbol)) {
    const side = p.size > 0n ? 'buy' : 'sell';
    const pnlText = `${formatUsd(p.unrealised, { sign: 'always' })} USDC`;
    levels.push({ id: `position:${p.symbol}`, kind: 'position', side, price: pxNumber(p.entryPx), priceText: formatPx(p.entryPx, dp), label: `${side === 'buy' ? 'Long' : 'Short'} ${formatSize(p.size < 0n ? -p.size : p.size, sd)} ${market.base}`, detail: pnlText, detailTone: p.unrealised > 0n ? 'up' : p.unrealised < 0n ? 'down' : 'neutral' });
    if (p.liquidationPx !== null) levels.push({ id: `liquidation:${p.symbol}`, kind: 'liquidation', side, price: pxNumber(p.liquidationPx), priceText: formatPx(p.liquidationPx, dp), label: 'Liquidation' });
  }
  for (const o of account.orders.filter((x) => x.symbol === market.symbol)) {
    const left = o.size - o.filled;
    const side = o.buy ? 'buy' : 'sell';
    if (o.status === 'untriggered' && o.triggerPx !== null)
      levels.push({ id: o.id, kind: 'trigger', side, price: pxNumber(o.triggerPx), priceText: formatPx(o.triggerPx, dp), label: `${o.type.startsWith('stop') ? 'Stop' : 'Take profit'} ${formatSize(left, sd)}`, detail: o.reduceOnly ? 'reduce-only' : undefined, detailTone: 'neutral' });
    else if (o.px !== null) levels.push({ id: o.id, kind: 'order', side, price: pxNumber(o.px), priceText: formatPx(o.px, dp), label: `${o.buy ? 'Buy' : 'Sell'} ${formatSize(left, sd)}` });
  }
  for (const f of account.fills.filter((x) => x.symbol === market.symbol).slice(0, 200))
    fills.push({ id: f.id, time: f.time, price: pxNumber(f.px), side: f.buy ? 'buy' : 'sell', liquidation: f.liquidation, text: `${f.buy ? 'Bought' : 'Sold'} ${formatSize(f.size, sd)} ${market.base} at ${formatPx(f.px, dp)}` });
  return { levels, fills };
}

const PriceChart = (chart as unknown as { PriceChart?: ComponentType<PriceChartProps> }).PriceChart;

export function ChartPanel({ market, ticker }: { market: Market; ticker: Ticker | undefined }) {
  const candles = useStore(feed.candles);
  const connection = useStore(feed.connection);
  const account = useStore(feed.account);
  const interval = useStore(chartInterval);
  const [theme] = useTheme();
  const { levels, fills } = chartInputs(market, account);
  const halted = market.status === 'halted';
  const notice: ChartNotice | null = halted
    ? { tone: 'danger', title: 'Market halted', text: market.haltReason ?? 'New orders are not accepted until it reopens.' }
    : connection.state !== 'open'
      ? { tone: 'warn', title: 'Reconnecting', text: 'Prices may be behind the exchange.' }
      : null;
  const dp = market.priceDecimals;
  if (PriceChart)
    return (
      <section className="panel chart-panel" aria-label="Price chart">
        <PriceChart
          symbol={market.symbol}
          base={market.base}
          interval={interval}
          intervals={INTERVALS}
          onIntervalChange={(i) => chartInterval.set(i)}
          candles={candles.symbol === market.symbol ? candles.candles : []}
          ready={candles.ready && candles.symbol === market.symbol}
          priceDecimals={dp}
          sizeDecimals={market.sizeDecimals}
          tickSize={pxNumber(market.tick)}
          mark={ticker ? { price: pxNumber(ticker.mark), text: formatPx(ticker.mark, dp) } : null}
          index={ticker ? { price: pxNumber(ticker.index), text: formatPx(ticker.index, dp) } : null}
          levels={levels}
          fills={fills}
          notice={notice}
          stale={!!notice}
          theme={theme}
          onPricePick={(p) => {
            // The chart gives a float; snap it to the tick and hand the exact text to the form.
            const scaled = BigInt(Math.round(p * 10 ** dp)) * 10n ** BigInt(PX_DECIMALS - dp);
            const ticked = scaled - (scaled % market.tick);
            pricePick.set({ text: toDecimalString(ticked / 10n ** BigInt(PX_DECIMALS - dp), dp), at: Date.now() });
          }}
        />
      </section>
    );
  const last = candles.candles[candles.candles.length - 1];
  return (
    <section className="panel chart-panel chart-pending" aria-label="Price chart">
      <header className="panel-head">
        <h2>{market.symbol}</h2>
        <div className="segmented" role="group" aria-label="Candle interval">
          {INTERVALS.map((i) => (
            <button key={i} type="button" aria-pressed={i === interval} onClick={() => chartInterval.set(i)}>
              {i}
            </button>
          ))}
        </div>
      </header>
      <div className="chart-placeholder">
        {notice && <p className={`badge badge-${notice.tone === 'danger' ? 'danger' : 'warn'}`}>{notice.title}</p>}
        <p className="eyebrow">Chart component pending (agent 396)</p>
        {last ? (
          <dl className="ohlc num">
            <div><dt>O</dt><dd>{last.o.toFixed(dp)}</dd></div>
            <div><dt>H</dt><dd>{last.h.toFixed(dp)}</dd></div>
            <div><dt>L</dt><dd>{last.l.toFixed(dp)}</dd></div>
            <div><dt>C</dt><dd>{last.c.toFixed(dp)}</dd></div>
          </dl>
        ) : (
          <p>Reading candles…</p>
        )}
        <p className="chart-levels">{levels.map((l) => `${l.label} ${l.priceText}`).join(' · ')}</p>
      </div>
    </section>
  );
}
