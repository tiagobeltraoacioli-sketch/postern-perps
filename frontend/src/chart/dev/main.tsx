/**
 * Chart harness: SYNTHETIC data (a deterministic random walk), never market data. Query parameters:
 *   theme=dark|light  scenario=normal|levels|far|halt|stale|empty|loading|gaps|crowded|solid|thin|zoomed
 *   n=<candles>  busy=<messages per second>  reduced=1
 * window.__chart exposes the frame statistics and a busy-feed driver to the measurement script.
 */
import { StrictMode, useEffect, useMemo, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import '@fontsource-variable/inter';
import '@fontsource-variable/fraunces';
import { PriceChart, chartStats, resetChartStats, type ChartLevel, type ChartFill, type ChartCandle, type ChartInterval } from '../index';
import type { ChartNotice } from '../types';
import { TOKEN_FALLBACK } from '../palette';
import { walk } from '../test/fixtures';

const q = new URLSearchParams(location.search);
const theme = q.get('theme') === 'light' ? 'light' : 'dark';
const scenario = q.get('scenario') ?? 'levels';
const n = Number(q.get('n') ?? 600);
document.documentElement.dataset.theme = theme;

// The app's tokens are not part of this branch; the harness writes the committed values the chart falls back to.
const t = TOKEN_FALLBACK[theme];
const extra = theme === 'dark'
  ? { '--surface-2': '#171c1e', '--surface-3': '#1f2629', '--focus': '#eceae3', '--info': '#9dbbea' }
  : { '--surface-2': '#f0ede4', '--surface-3': '#e6e2d6', '--focus': '#14191a', '--info': '#245c9f' };
const vars: Record<string, string> = {
  '--bg': t.bg, '--surface': t.surface, '--line': t.grid, '--line-strong': t.axisLine, '--text': t.text, '--text-2': t.text2,
  '--text-3': t.text3, '--net': t.net, '--warn': t.warn, '--danger': t.danger, '--ok': t.ok, ...extra,
  '--font-ui': "'Inter Variable', system-ui, sans-serif", '--font-display': "'Fraunces Variable', Georgia, serif",
  '--s-1': '4px', '--s-2': '8px', '--s-3': '12px', '--s-4': '16px', '--r-1': '4px', '--r-2': '8px', '--r-arch': '999px',
  '--text-micro': '0.75rem', '--text-small': '0.8125rem', '--text-ui': '0.875rem', '--t-fast': '120ms', '--ease': 'ease',
  '--shadow-overlay': theme === 'dark' ? '0 24px 64px -16px rgb(0 0 0 / 0.7)' : '0 24px 64px -20px rgb(20 25 26 / 0.35)',
};
const style = document.createElement('style');
style.textContent = `:root{${Object.entries(vars).map(([k, v]) => `${k}:${v}`).join(';')};color-scheme:${theme}}
html,body{margin:0;height:100%;background:var(--bg);color:var(--text);font-family:var(--font-ui)}
#root{height:100%;display:flex;flex-direction:column}
.frame{flex:1;min-height:0;padding:12px;box-sizing:border-box;display:flex}
.frame>.pc{flex:1;border:1px solid var(--line);border-radius:8px;overflow:hidden}`;
document.head.appendChild(style);

const M15 = 900_000;
const START = Date.UTC(2026, 9, 11, 0, 0); // synthetic history start
const INTERVALS: Record<ChartInterval, number> = { '1m': 60_000, '5m': 300_000, '15m': M15, '1h': 3_600_000, '4h': 14_400_000, '1d': 86_400_000 };

type Busy = { rate: number; until: number };
const busy: { current: Busy | null } = { current: null };
let messages = 0;

function App() {
  const [interval, setInterval_] = useState<ChartInterval>((q.get('interval') as ChartInterval) ?? '15m');
  const ms = INTERVALS[interval];
  const base = useMemo(() => {
    let cs = walk(n, START + (600 - n) * M15, ms);
    if (scenario === 'gaps') cs = cs.filter((_, i) => i % 23 !== 5 && !(i > n - 40 && i < n - 33));
    return cs;
  }, [ms]);
  const [candles, setCandles] = useState<readonly ChartCandle[]>(base);
  const [picked, setPicked] = useState<number | null>(null);
  const queue = useRef<number[]>([]);
  useEffect(() => setCandles(base), [base]);

  // Busy feed: messages arrive on timers at `rate` per second and are applied once per frame (as the app's feed).
  useEffect(() => {
    let raf = 0;
    let timer = 0;
    const tick = () => {
      const b = busy.current;
      if (b && performance.now() < b.until) {
        const k = Math.max(1, Math.round(b.rate / 250));
        for (let i = 0; i < k; i++) {
          messages++;
          queue.current.push(Math.sin(messages / 9) * 6 + ((messages * 7919) % 13) - 6);
        }
      }
      timer = window.setTimeout(tick, 4);
    };
    const flush = () => {
      if (queue.current.length) {
        const moves = queue.current.splice(0);
        setCandles((cs) => {
          const last = cs[cs.length - 1];
          let { h, l, c } = last;
          for (const m of moves) {
            c = Math.round((c + m) * 10) / 10;
            h = Math.max(h, c);
            l = Math.min(l, c);
          }
          const roll = messages % 600 < moves.length; // a new candle every ~600 messages
          if (roll) return [...cs, { t: last.t + ms, o: c, h: c, l: c, c, v: 0.01 }].slice(-1500);
          return [...cs.slice(0, -1), { ...last, h, l, c, v: Math.round((last.v + moves.length * 0.01) * 100) / 100 }];
        });
      }
      raf = requestAnimationFrame(flush);
    };
    tick();
    raf = requestAnimationFrame(flush);
    return () => {
      clearTimeout(timer);
      cancelAnimationFrame(raf);
    };
  }, [ms]);

  const last = candles[candles.length - 1];
  const fmt = (v: number) => v.toLocaleString('en-US', { minimumFractionDigits: 1, maximumFractionDigits: 1 });
  const markP = last ? Math.round((last.c + 4.3) * 10) / 10 : 0;
  const indexP = last ? Math.round((last.c - 6.1) * 10) / 10 : 0;
  const entry = base[base.length - 60]?.c ?? 0;
  const liqP = scenario === 'far' ? Math.round(entry * 0.62 * 10) / 10 : Math.round((entry - (last ? Math.abs(last.c - entry) + 380 : 0)) * 10) / 10;
  const withLevels = !['normal', 'empty', 'loading', 'thin'].includes(scenario);
  const levels: ChartLevel[] = withLevels && last
    ? [
        { id: 'position:BTC-USD', kind: 'position', side: 'buy', price: entry, priceText: fmt(entry), label: 'Long 0.5000 BTC', detail: `${last.c >= entry ? '+' : '−'}${fmt(Math.abs((last.c - entry) * 0.5))} USDC`, detailTone: last.c >= entry ? 'up' : 'down' },
        { id: 'liquidation:BTC-USD', kind: 'liquidation', side: 'buy', price: liqP, priceText: fmt(liqP), label: 'Liquidation' },
        { id: 'o1', kind: 'order', side: 'sell', price: Math.round((last.c + 220) * 10) / 10, priceText: fmt(Math.round((last.c + 220) * 10) / 10), label: 'Sell 0.2500', detail: 'reduce-only', detailTone: 'neutral' },
        { id: 'o2', kind: 'order', side: 'buy', price: Math.round((last.c - 260) * 10) / 10, priceText: fmt(Math.round((last.c - 260) * 10) / 10), label: 'Buy 0.1000' },
        { id: 't1', kind: 'trigger', side: 'sell', price: Math.round((entry - 150) * 10) / 10, priceText: fmt(Math.round((entry - 150) * 10) / 10), label: 'Stop 0.5000' },
        ...(scenario === 'crowded'
          ? Array.from({ length: 14 }, (_, i): ChartLevel => ({ id: `c${i}`, kind: 'order', side: i % 2 ? 'buy' : 'sell', price: Math.round((last.c + (i - 7) * 9) * 10) / 10, priceText: fmt(Math.round((last.c + (i - 7) * 9) * 10) / 10), label: `${i % 2 ? 'Buy' : 'Sell'} 0.0${i + 1}00` }))
          : []),
      ]
    : [];
  const fills: ChartFill[] = withLevels && base.length > 70
    ? [
        { id: 'f1', time: base[base.length - 60].t + 120_000, price: entry, side: 'buy', text: `Bought 0.5000 BTC at ${fmt(entry)}` },
        { id: 'f2', time: base[base.length - 25].t + 60_000, price: base[base.length - 25].h, side: 'sell', text: 'Sold' },
      ]
    : [];
  const notice: ChartNotice | null = scenario === 'halt'
    ? { tone: 'danger', title: 'Market halted', text: 'The oracle is unsafe: two of three sources are stale. Orders are refused; cancels work.' }
    : scenario === 'stale'
      ? { tone: 'warn', title: 'Reconnecting', text: 'The chart may be behind the exchange.' }
      : null;
  const shown = scenario === 'empty' || scenario === 'loading' ? [] : candles;

  return (
    <StrictMode>
      <div className="frame">
        <PriceChart
          symbol="BTC-USD"
          base="BTC"
          interval={interval}
          candles={shown}
          ready={scenario !== 'loading'}
          priceDecimals={1}
          sizeDecimals={4}
          tickSize={0.5}
          mark={last && scenario !== 'empty' ? { price: markP, text: fmt(markP) } : null}
          index={last && scenario !== 'empty' ? { price: indexP, text: fmt(indexP) } : null}
          levels={levels}
          fills={fills}
          notice={notice}
          stale={scenario === 'stale' || scenario === 'halt'}
          onIntervalChange={setInterval_}
          onPricePick={setPicked}
          candleStyle={scenario === 'solid' ? 'solid' : 'hollow'}
          reducedMotion={q.get('reduced') === '1' ? true : undefined}
          theme={theme}
        />
      </div>
      <output id="picked" hidden>{picked ?? ''}</output>
    </StrictMode>
  );
}

(window as unknown as { __chart: unknown }).__chart = {
  chartStats,
  resetChartStats,
  startBusy(rate: number, seconds: number) {
    messages = 0;
    busy.current = { rate, until: performance.now() + seconds * 1000 };
  },
  messages: () => messages,
};

createRoot(document.getElementById('root')!).render(<App />);
