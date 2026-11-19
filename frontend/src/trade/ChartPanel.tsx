/**
 * The chart slot. The chart itself is agent 396's component (`frontend/src/chart/`, interface v0.1.0); this file only
 * turns the app's exact values into its props (`chartInputs`, tested). Exact prices go as the app's fixed-point
 * text; a price picked on the chart is a float, snapped here to the tick with the app's own integer rule.
 */
import { PriceChart, type ChartInterval, type ChartLevel, type ChartFill, type ChartNotice } from '../chart';
import { feed } from '../state/session';
import { useStore, persisted } from '../lib/store';
import { formatPx, formatSize, formatUsd } from '../lib/format';
import { pxNumber } from '../lib/convert';
import type { AccountState } from '../lib/feed';
import type { Market, Ticker } from '../lib/types';
import { pricePick } from './hooks';
import { useTheme } from '../ui/theme';
import { toDecimalString, PX_DECIMALS } from '../lib/fixed';

/** One empty array, so the chart sees the same reference while another market's candles are cleared. */
const NO_CANDLES: readonly never[] = [];
const INTERVALS: ChartInterval[] = ['1m', '5m', '15m', '1h', '4h', '1d'];
/** Moving averages on the chart (display only): off until the user turns them on. */
export const chartAverages = persisted<boolean>('postern.perps.chart.ma.v1', false, (v): v is boolean => typeof v === 'boolean');
/** Periods of the two averages: 20 and 50 candles of the chosen interval. */
export const AVERAGE_PERIODS: readonly number[] = [20, 50];
const NO_AVERAGES: readonly number[] = [];
/** A size without its trailing zeros, for the chart's short labels ("0.5000" reads "0.5"). */
export function trimZeros(text: string): string {
  return text.includes('.') ? text.replace(/0+$/, '').replace(/\.$/, '') : text;
}
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
    levels.push({ id: `position:${p.symbol}`, kind: 'position', side, price: pxNumber(p.entryPx), priceText: formatPx(p.entryPx, dp), label: `${side === 'buy' ? 'Long' : 'Short'} ${formatSize(p.size < 0n ? -p.size : p.size, sd)} ${market.base}`, short: `${side === 'buy' ? 'Long' : 'Short'} ${trimZeros(formatSize(p.size < 0n ? -p.size : p.size, sd))}`, detail: pnlText, detailTone: p.unrealised > 0n ? 'up' : p.unrealised < 0n ? 'down' : 'neutral' });
    if (p.liquidationPx !== null) levels.push({ id: `liquidation:${p.symbol}`, kind: 'liquidation', side, price: pxNumber(p.liquidationPx), priceText: formatPx(p.liquidationPx, dp), label: 'Liquidation', short: 'Liq.' });
  }
  for (const o of account.orders.filter((x) => x.symbol === market.symbol)) {
    const left = o.size - o.filled;
    const side = o.buy ? 'buy' : 'sell';
    if (o.status === 'untriggered' && o.triggerPx !== null)
      levels.push({ id: o.id, kind: 'trigger', side, price: pxNumber(o.triggerPx), priceText: formatPx(o.triggerPx, dp), label: `${o.type.startsWith('stop') ? 'Stop' : 'Take profit'} ${formatSize(left, sd)}`, short: `${o.type.startsWith('stop') ? 'Stop' : 'TP'} ${trimZeros(formatSize(left, sd))}`, detail: o.reduceOnly ? 'reduce-only' : undefined, detailTone: 'neutral' });
    else if (o.px !== null) levels.push({ id: o.id, kind: 'order', side, price: pxNumber(o.px), priceText: formatPx(o.px, dp), label: `${o.buy ? 'Buy' : 'Sell'} ${formatSize(left, sd)}`, short: `${o.buy ? 'Buy' : 'Sell'} ${trimZeros(formatSize(left, sd))}` });
  }
  for (const f of account.fills.filter((x) => x.symbol === market.symbol).slice(0, 200))
    fills.push({ id: f.id, time: f.time, price: pxNumber(f.px), side: f.buy ? 'buy' : 'sell', liquidation: f.liquidation, text: `${f.buy ? 'Bought' : 'Sold'} ${formatSize(f.size, sd)} ${market.base} at ${formatPx(f.px, dp)}` });
  return { levels, fills };
}


export function ChartPanel({ market, ticker }: { market: Market; ticker: Ticker | undefined }) {
  const candles = useStore(feed.candles);
  const connection = useStore(feed.connection);
  const account = useStore(feed.account);
  const interval = useStore(chartInterval);
  const averagesOn = useStore(chartAverages);
  const [theme] = useTheme();
  const { levels, fills } = chartInputs(market, account);
  const halted = market.status === 'halted';
  const notice: ChartNotice | null = halted
    ? { tone: 'danger', title: 'Market halted', text: market.haltReason ?? 'New orders are not accepted until it reopens.' }
    : connection.state !== 'open'
      ? { tone: 'warn', title: 'Reconnecting', text: 'Prices may be behind the exchange.' }
      : null;
  const dp = market.priceDecimals;
  return (
      <section className="panel chart-panel" aria-label="Price chart">
        <PriceChart
          symbol={market.symbol}
          base={market.base}
          interval={interval}
          intervals={INTERVALS}
          onIntervalChange={(i) => chartInterval.set(i)}
          averages={averagesOn ? AVERAGE_PERIODS : NO_AVERAGES}
          onAveragesToggle={(on) => chartAverages.set(on)}
          candles={candles.symbol === market.symbol ? candles.candles : NO_CANDLES}
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
}
