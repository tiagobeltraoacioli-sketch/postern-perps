/**
 * PriceChart: the React face of the in-house canvas chart (agent 396). UNAUDITED prototype work.
 *
 * Header: symbol, the candle readout (open, high, low, close, change, volume of the hovered candle, else the newest)
 * and the interval switch. Plot: two canvases (base and crosshair overlay), the notice drawn over the data, the
 * loading and empty states, a "Latest" button when the view has left the newest candle. Every price the user acts
 * on is also written, as the app formatted it, in a visually hidden summary tied to the plot (screen readers get
 * the liquidation price and the orders, not just a picture).
 */
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties, KeyboardEvent, MouseEvent, PointerEvent } from 'react';
import type { ChartInterval, PriceChartProps } from './types';
import { ChartEngine, type HoverInfo } from './engine';
import { undrawableLevels } from './levels';
import { ALL_INTERVALS, INTERVAL_MS } from './scale';
import { formatCandleTime, formatNumber, formatPercent, intervalWords } from './format';
import type { Theme } from './palette';
import './chart.css';

const EMPTY: readonly never[] = [];

function documentTheme(): Theme {
  return typeof document !== 'undefined' && document.documentElement.dataset.theme === 'light' ? 'light' : 'dark';
}

function useReducedMotion(forced: boolean | undefined): boolean {
  const [media, setMedia] = useState(() => {
    try {
      return window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false;
    } catch {
      return false;
    }
  });
  useEffect(() => {
    let mq: MediaQueryList | undefined;
    try {
      mq = window.matchMedia?.('(prefers-reduced-motion: reduce)');
    } catch {
      return;
    }
    if (!mq) return;
    const on = () => setMedia(mq!.matches);
    mq.addEventListener?.('change', on);
    return () => mq!.removeEventListener?.('change', on);
  }, []);
  return forced ?? media;
}

export function PriceChart(props: PriceChartProps) {
  const {
    symbol,
    base,
    interval,
    candles,
    ready,
    priceDecimals,
    sizeDecimals,
    tickSize,
    mark = null,
    index = null,
    levels = EMPTY,
    fills = EMPTY,
    notice = null,
    stale = false,
    intervals = ALL_INTERVALS,
    onIntervalChange,
    onReachStart,
    onPricePick,
    candleStyle = 'hollow',
    wheelZoom = true,
    theme,
    reducedMotion,
    label,
    className,
  } = props;

  const rootRef = useRef<HTMLDivElement>(null);
  const plotRef = useRef<HTMLDivElement>(null);
  const baseRef = useRef<HTMLCanvasElement>(null);
  const overlayRef = useRef<HTMLCanvasElement>(null);
  const engineRef = useRef<ChartEngine | null>(null);
  const hooksRef = useRef({ onReachStart, onPricePick });
  hooksRef.current = { onReachStart, onPricePick };

  const [hover, setHover] = useState<HoverInfo | null>(null);
  const [following, setFollowing] = useState(true);
  const [rejected, setRejected] = useState(0);
  const [inks, setInks] = useState<{ up: string; down: string } | null>(null);
  const motionReduced = useReducedMotion(reducedMotion);
  // R395-32: levels (and the live mark / index) whose price is not a finite number have no place on the canvas.
  // They are named on screen, never left out in silence.
  const undrawn = useMemo(
    () =>
      undrawableLevels<{ price: number; label: string; kind: string }>([
        ...levels,
        ...(mark ? [{ price: mark.price, label: 'Mark price', kind: 'mark' }] : []),
        ...(index ? [{ price: index.price, label: 'Index price', kind: 'index' }] : []),
      ]),
    [levels, mark, index],
  );
  const ids = useId();
  const summaryId = `${ids}-summary`;
  const keysId = `${ids}-keys`;

  // Engine lifetime.
  useLayoutEffect(() => {
    const baseCanvas = baseRef.current;
    const overlay = overlayRef.current;
    const root = rootRef.current;
    if (!baseCanvas || !overlay || !root) return;
    const engine = new ChartEngine(
      { base: baseCanvas, overlay, styleRoot: root },
      {
        onHover: setHover,
        onFollowChange: setFollowing,
        onReachStart: (t) => hooksRef.current.onReachStart?.(t),
        onPricePick: (p) => hooksRef.current.onPricePick?.(p),
        onPalette: (p) => setInks((cur) => (cur && cur.up === p.up && cur.down === p.down ? cur : { up: p.up, down: p.down })),
      },
    );
    engineRef.current = engine;
    engine.setTheme(theme ?? documentTheme());
    return () => {
      engine.destroy();
      engineRef.current = null;
    };
    // The engine is created once; props reach it through the effects below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Size and pixel ratio.
  useLayoutEffect(() => {
    const plot = plotRef.current;
    const engine = engineRef.current;
    if (!plot || !engine) return;
    const measure = () => {
      const r = plot.getBoundingClientRect();
      engine.resize(r.width, r.height, window.devicePixelRatio || 1);
    };
    measure();
    const ro = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(measure) : null;
    ro?.observe(plot);
    window.addEventListener('resize', measure);
    return () => {
      ro?.disconnect();
      window.removeEventListener('resize', measure);
    };
  }, []);

  // Theme: the prop when given; also follow <html data-theme> (the colours are read after the page applied it).
  useEffect(() => {
    const engine = engineRef.current;
    if (!engine) return;
    const apply = () => engine.setTheme(theme ?? documentTheme());
    apply();
    const id = requestAnimationFrame(apply);
    const mo = typeof MutationObserver !== 'undefined' ? new MutationObserver(apply) : null;
    mo?.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'data-network', 'class', 'style'] });
    return () => {
      cancelAnimationFrame(id);
      mo?.disconnect();
    };
  }, [theme]);

  useEffect(() => {
    if (engineRef.current) engineRef.current.reducedMotion = motionReduced;
  }, [motionReduced]);

  // Data.
  useLayoutEffect(() => {
    const engine = engineRef.current;
    if (!engine) return;
    engine.setData({
      symbol,
      interval,
      candles,
      priceDecimals,
      sizeDecimals,
      tickSize,
      mark,
      index,
      levels,
      fills,
      stale,
      candleStyle,
      wheelZoom,
      pickable: !!onPricePick,
    });
    setRejected(engine.getSeries().rejected);
  }, [symbol, interval, candles, priceDecimals, sizeDecimals, tickSize, mark, index, levels, fills, stale, candleStyle, wheelZoom, onPricePick]);

  // Wheel needs a non-passive listener to keep the page still while zooming.
  useEffect(() => {
    const el = overlayRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      const engine = engineRef.current;
      if (!engine) return;
      const r = el.getBoundingClientRect();
      const scale = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? r.height : 1;
      if (engine.wheel(e.clientX - r.left, e.deltaX * scale, e.deltaY * scale, e.ctrlKey)) e.preventDefault();
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  }, []);

  const local = (e: PointerEvent | MouseEvent) => {
    const r = overlayRef.current!.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top] as const;
  };
  const onPointerDown = (e: PointerEvent) => {
    if (e.button !== 0 && e.pointerType === 'mouse') return;
    const [x, y] = local(e);
    try {
      overlayRef.current?.setPointerCapture?.(e.pointerId);
    } catch {
      /* capture unavailable: dragging still works inside the canvas */
    }
    engineRef.current?.pointerDown(e.pointerId, x, y);
  };
  const onPointerMove = (e: PointerEvent) => {
    const [x, y] = local(e);
    engineRef.current?.pointerMove(e.pointerId, x, y);
  };
  const onPointerUp = (e: PointerEvent) => engineRef.current?.pointerUp(e.pointerId);
  const onPointerLeave = (e: PointerEvent) => {
    if (e.pointerType === 'mouse') engineRef.current?.pointerLeave();
  };
  const onDoubleClick = (e: MouseEvent) => engineRef.current?.doubleClick(local(e)[0]);
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.altKey || e.metaKey || e.ctrlKey) return;
    if (engineRef.current?.key(e.key, e.shiftKey)) e.preventDefault();
  };
  const onBlur = () => engineRef.current?.pointerLeave();
  const goLatest = useCallback(() => engineRef.current?.goToLatest(), []);

  // Readout: hovered candle, else the newest.
  const newest = candles.length ? candles[candles.length - 1] : null;
  const shown = hover?.hovering ? hover.candle : newest;
  const shownTime = hover?.hovering ? hover.slotTime : (newest?.t ?? null);
  const intervalMs = INTERVAL_MS[interval];
  const readout = shown
    ? {
        o: formatNumber(shown.o, priceDecimals),
        h: formatNumber(shown.h, priceDecimals),
        l: formatNumber(shown.l, priceDecimals),
        c: formatNumber(shown.c, priceDecimals),
        chg: shown.o > 0 ? formatPercent((shown.c - shown.o) / shown.o) : '',
        up: shown.c >= shown.o,
        v: formatNumber(shown.v, sizeDecimals),
      }
    : null;

  const name = label ?? `${symbol} price chart`;
  const summary = useMemo(() => {
    const parts: string[] = [`${intervalWords(intervalMs)} candles, times in UTC.`];
    if (newest) {
      parts.push(
        `Newest candle opened ${formatCandleTime(newest.t, intervalMs)}: open ${formatNumber(newest.o, priceDecimals)}, high ${formatNumber(newest.h, priceDecimals)}, low ${formatNumber(newest.l, priceDecimals)}, close ${formatNumber(newest.c, priceDecimals)}.`,
      );
    }
    if (mark) parts.push(`Mark price ${mark.text}.`);
    if (index) parts.push(`Index price ${index.text}.`);
    if (stale) parts.push('The chart may be behind the exchange.');
    return parts.join(' ');
  }, [intervalMs, newest, mark, index, stale, priceDecimals]);

  const state = !ready ? 'loading' : candles.length === 0 ? 'empty' : null;

  return (
    <div
      ref={rootRef}
      className={`pc${className ? ` ${className}` : ''}`}
      data-stale={stale || undefined}
      data-motion={motionReduced ? 'reduced' : undefined}
      style={inks ? ({ '--pc-up': inks.up, '--pc-down': inks.down } as CSSProperties) : undefined}
    >
      <div className="pc-head">
        <div className="pc-readout" aria-hidden="true">
          <span className="pc-symbol">{symbol}</span>
          {readout ? (
            <>
              <span className="pc-ro">
                <i>O</i>
                {readout.o}
              </span>
              <span className="pc-ro">
                <i>H</i>
                {readout.h}
              </span>
              <span className="pc-ro">
                <i>L</i>
                {readout.l}
              </span>
              <span className="pc-ro">
                <i>C</i>
                <b data-dir={readout.up ? 'up' : 'down'}>{readout.c}</b>
              </span>
              {readout.chg && (
                <span className="pc-ro" data-dir={readout.up ? 'up' : 'down'}>
                  {readout.chg}
                </span>
              )}
              <span className="pc-ro">
                <i>V</i>
                {readout.v}
                {base ? ` ${base}` : ''}
              </span>
            </>
          ) : hover?.hovering && shownTime !== null ? (
            <span className="pc-ro pc-gap">No trades in this candle</span>
          ) : null}
          {shownTime !== null && <span className="pc-time">{formatCandleTime(shownTime, intervalMs)} UTC</span>}
          {stale && <span className="pc-stale">stale</span>}
        </div>
        {onIntervalChange && (
          <div className="pc-intervals" role="group" aria-label="Candle interval">
            {intervals.map((iv: ChartInterval) => (
              <button
                key={iv}
                type="button"
                className="pc-iv"
                aria-pressed={iv === interval}
                onClick={() => iv !== interval && onIntervalChange(iv)}
              >
                {iv}
              </button>
            ))}
          </div>
        )}
      </div>
      <div
        ref={plotRef}
        className="pc-plot"
        role="group"
        aria-roledescription="chart"
        aria-label={name}
        aria-describedby={`${summaryId} ${keysId}`}
        tabIndex={0}
        onKeyDown={onKeyDown}
        onBlur={onBlur}
      >
        <canvas ref={baseRef} className="pc-canvas" aria-hidden="true" />
        <canvas
          ref={overlayRef}
          className="pc-canvas pc-overlay"
          aria-hidden="true"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onPointerLeave={onPointerLeave}
          onDoubleClick={onDoubleClick}
        />
        {state === 'loading' && (
          <div className="pc-state" role="status">
            <span className="pc-skeleton" aria-hidden="true" />
            Reading candles…
          </div>
        )}
        {state === 'empty' && (
          <div className="pc-state" role="status">
            No candles yet for this market and interval.
          </div>
        )}
        {notice && (
          <div className="pc-notice" data-tone={notice.tone} role={notice.tone === 'danger' ? 'alert' : 'status'}>
            <strong>{notice.title}</strong>
            {notice.text && <span>{notice.text}</span>}
          </div>
        )}
        {!following && candles.length > 0 && (
          <button type="button" className="pc-latest" onClick={goLatest}>
            Latest
            <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
              <path d="M3 8h9M8.5 4.5 12 8l-3.5 3.5" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          </button>
        )}
        {undrawn.length > 0 && (
          <div className="pc-undrawn" role={undrawn.some((l) => l.kind === 'liquidation') ? 'alert' : 'status'}>
            <strong>
              {undrawn.length} {undrawn.length === 1 ? 'level' : 'levels'} not drawn: invalid price
            </strong>
            <span>{undrawn.map((l) => l.label).join(', ')}</span>
          </div>
        )}
        {rejected > 0 && (
          <div className="pc-rejected" role="status">
            {rejected} {rejected === 1 ? 'candle' : 'candles'} not drawn: invalid data from the feed.
          </div>
        )}
      </div>
      <div className="pc-sr" id={summaryId}>
        <p>{summary}</p>
        {levels.length > 0 && (
          <ul aria-label="Your levels">
            {levels.map((l) => (
              <li key={l.id}>
                {l.label} at {l.priceText}
                {l.detail ? `, ${l.detail}` : ''}
              </li>
            ))}
          </ul>
        )}
      </div>
      <p className="pc-sr" id={keysId}>
        Keys: left and right arrows move in time (with Shift, ten candles), plus and minus zoom, Home and End go to the
        oldest and newest candle, up and down arrows choose a price
        {onPricePick ? ', Enter puts that price in the order form' : ''}, Escape clears it.
      </p>
    </div>
  );
}

