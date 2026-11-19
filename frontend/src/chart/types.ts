/**
 * Public interface of the PosternDEX Perpetuals price chart (agent 396). UNAUDITED prototype work.
 *
 * The chart is a drawing of data it is given and nothing else: it never invents, fills, interpolates or tweens a
 * value. A missing candle is an empty slot, not a flat candle; a level without a price is not drawn; an animated
 * transition only ever moves the view (scale, opacity), never a price. Exact figures the user acts on (mark,
 * index, entry, liquidation and order prices) arrive as text formatted by the app from its fixed-point integers
 * (`priceText`); the chart formats only its own geometry (axis ticks, the crosshair coordinate) and the candle
 * readout, from the numbers it was given, at the market's decimals.
 *
 * Interface version: see CHART_INTERFACE_VERSION in ./index.ts. Changes are announced in the perps cross-area log.
 */

/** Candle intervals of docs/API.md (`GET /v1/markets/{symbol}/candles`, WebSocket `candles`). */
export type ChartInterval = '1m' | '5m' | '15m' | '1h' | '4h' | '1d';

/**
 * One candle, structurally identical to the frontend's parsed `Candle` (src/lib/types.ts): `t` is the open time in
 * milliseconds (docs/API.md `Candle.t`), `o h l c` are prices and `v` is the base-asset volume, as numbers (candles
 * are geometry). Oldest first, one interval, distinct `t`; the last one may still be open.
 */
export type ChartCandle = {
  readonly t: number;
  readonly o: number;
  readonly h: number;
  readonly l: number;
  readonly c: number;
  readonly v: number;
};

/** A price the app knows exactly: `price` places it, `text` is what is written (the app's formatPx output). */
export type ChartPrice = { readonly price: number; readonly text: string };

/**
 * A horizontal level drawn across the plot with a label at its left and a tag on the price axis.
 *
 * - `position`: the user's open position at its average entry price (`side` buy = long, sell = short).
 * - `liquidation`: that position's liquidation price as the exchange reports it (never computed by the chart);
 *   `side` is the position's side, which tells the chart on which side of the line the shaded zone lies.
 * - `order`: a resting limit order at its limit price.
 * - `trigger`: an untriggered stop or take-profit order at its trigger price.
 *
 * A level whose price lies outside the visible price range stays on screen: its tag is pinned to the top or bottom
 * edge of the price axis with an arrow, so a liquidation price is never out of sight.
 */
export type ChartLevelKind = 'position' | 'liquidation' | 'order' | 'trigger';
export type ChartLevel = {
  /** Stable across updates (order id, `position:BTC-USD`, `liquidation:BTC-USD`). */
  readonly id: string;
  readonly kind: ChartLevelKind;
  readonly side: 'buy' | 'sell';
  readonly price: number;
  /** The exact price as the app formats it; written in the axis tag. */
  readonly priceText: string;
  /** The line's label, formatted by the app: "Long 0.5000 BTC", "Buy 0.2500", "Stop 0.1000", "Liquidation". */
  readonly label: string;
  /** A shorter label for a narrow plot, formatted by the app: "Long 0.5", "Buy 0.25", "Liq.". Default: `label`. */
  readonly short?: string;
  /** Optional second text, formatted by the app: unrealised PnL "+225.60 USDC", "mark below", "reduce-only". */
  readonly detail?: string;
  /** Colour of `detail`: up and down use the chart's up and down colours; neutral the secondary text colour. */
  readonly detailTone?: 'up' | 'down' | 'neutral';
};

/** One of the user's fills, drawn as a marker at its price in the candle of its time. */
export type ChartFill = {
  readonly id: string;
  /** Fill time, ms. */
  readonly time: number;
  readonly price: number;
  readonly side: 'buy' | 'sell';
  /** True for a fill of a liquidation of the user's position (drawn in the danger colour). */
  readonly liquidation?: boolean;
  /** Accessible text of the marker, formatted by the app: "Bought 0.1000 BTC at 66,900.0". */
  readonly text?: string;
};

/** A state shown over the plot (the data stays visible under it): a halted market, a stale or reconnecting feed. */
export type ChartNotice = {
  readonly tone: 'info' | 'warn' | 'danger';
  readonly title: string;
  readonly text?: string;
};

export type CandleStyle = 'hollow' | 'solid';

export type PriceChartProps = {
  /** Market symbol (`BTC-USD`). A change resets the view. */
  symbol: string;
  /** Base asset of the market (`BTC`): the unit of the volume readout. */
  base?: string;
  interval: ChartInterval;
  /** Oldest first, one interval (Feed.candles.candles). May be replaced or grow by one per update. */
  candles: readonly ChartCandle[];
  /** False until the candle snapshot has arrived (Feed.candles.ready): the chart shows that it is reading. */
  ready: boolean;
  /** Market price decimals (Market.priceDecimals): axis ticks, crosshair and candle readout. */
  priceDecimals: number;
  /** Market size decimals (Market.sizeDecimals): volume readout. */
  sizeDecimals: number;
  /** Market tick size as a number (Market.tick at the price scale): the crosshair price snaps to it. */
  tickSize?: number;
  /** Mark price (Ticker.mark); null or absent: no mark line. */
  mark?: ChartPrice | null;
  /** Oracle index price (Ticker.index); null or absent: no index line. */
  index?: ChartPrice | null;
  /** The user's position, liquidation and order levels in this market. */
  levels?: readonly ChartLevel[];
  /** The user's fills in this market. */
  fills?: readonly ChartFill[];
  /** Shown over the plot with the data still visible. */
  notice?: ChartNotice | null;
  /**
   * True when the last candle and the mark may be behind the exchange (feed reconnecting, market halted): the live
   * marks are drawn muted and the readout says "stale". The app also passes a notice saying why.
   */
  stale?: boolean;
  /** Intervals offered by the chart's interval switch (default: all six). Shown only with onIntervalChange. */
  intervals?: readonly ChartInterval[];
  onIntervalChange?: (interval: ChartInterval) => void;
  /** Called once per oldest candle when the user pans to the start of the history (older candles: REST `to=`). */
  onReachStart?: (oldestT: number) => void;
  /** Called when the user picks a price on the price axis (the + next to the crosshair tag, or Enter): a limit price. */
  onPricePick?: (price: number) => void;
  /** Hollow up candles (default) carry the direction by shape as well as colour; solid fills both. */
  candleStyle?: CandleStyle;
  /**
   * Periods of simple moving averages of the closes, drawn as lines and named in the readout as display-only
   * (average.ts): at most three whole numbers from 2 to 400. Absent or empty: none.
   */
  averages?: readonly number[];
  /** With it the chart offers a switch for the averages (pressed when `averages` is not empty). */
  onAveragesToggle?: (on: boolean) => void;
  /** Vertical wheel zooms the chart (default true). False: only ctrl/pinch zooms and the page scrolls. */
  wheelZoom?: boolean;
  /** Current theme (useTheme()[0]); a change re-reads the colours. Without it the chart watches <html data-theme>. */
  theme?: 'dark' | 'light';
  /** Forces reduced motion on or off; default: the `prefers-reduced-motion` media query, followed live. */
  reducedMotion?: boolean;
  /** Accessible name; default `${symbol} price chart`. */
  label?: string;
  className?: string;
};

/** Frame statistics of the chart (work per frame), read by a performance page and the measurement harness. */
export type ChartFrameStats = {
  /** Frames drawn (base layer, overlay layer). */
  baseFrames: number;
  overlayFrames: number;
  /** Duration of each drawn frame's work in ms (base + overlay of that frame), newest last, at most 20 000. */
  frameMs: number[];
  /** Updates received (setters called) and frames they were coalesced into. */
  updates: number;
};
