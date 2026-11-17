/**
 * Text the chart writes itself: axis ticks, the crosshair coordinate, the candle readout. Exact figures the user
 * acts on (mark, index, entry, liquidation, orders) arrive as text from the app and are never re-formatted here.
 *
 * Numbers: fixed decimals, thousands grouped with ',', minus U+2212 (as the app's format.ts). For a value given as
 * a decimal with at most 15 significant digits (every candle price parsed from the API's decimal strings),
 * `formatNumber(v, d)` with `d` at least the value's own places writes exactly that decimal: the double nearest to
 * it is within half an ulp, far below half a unit of the last place (test: format.test.ts "exact").
 * Times: UTC, as the rest of the app.
 */
export const MINUS = '−';

const GROUP = /\B(?=(\d{3})+(?!\d))/g;

/** `value` with `decimals` places (half away from zero on the double's exact value), grouped, U+2212 minus. */
export function formatNumber(value: number, decimals: number, grouping = true): string {
  if (!Number.isFinite(value)) return '';
  const d = Math.max(0, Math.min(20, Math.floor(decimals)));
  const fixed = Math.abs(value).toFixed(d);
  const negative = value < 0 && /[1-9]/.test(fixed);
  const point = fixed.indexOf('.');
  const whole = point < 0 ? fixed : fixed.slice(0, point);
  const frac = point < 0 ? '' : fixed.slice(point);
  return (negative ? MINUS : '') + (grouping ? whole.replace(GROUP, ',') : whole) + frac;
}

/** A signed percentage with two places: +0.02%, −1.25%, 0.00%. */
export function formatPercent(fraction: number): string {
  if (!Number.isFinite(fraction)) return '';
  const body = formatNumber(fraction * 100, 2);
  return body.startsWith(MINUS) || /^0(\.0+)?$/.test(body) ? `${body}%` : `+${body}%`;
}

/** Decimal places needed to write multiples of `step` exactly (step 0.5 -> 1, 25 -> 0, 0.0025 -> 4), at most 12. */
export function decimalsOf(step: number): number {
  if (!(step > 0) || !Number.isFinite(step)) return 0;
  for (let d = 0; d <= 12; d++) {
    const scaled = step * 10 ** d;
    if (Math.abs(scaled - Math.round(scaled)) < 1e-9 * Math.max(1, scaled)) return d;
  }
  return 12;
}

const two = (n: number) => (n < 10 ? `0${n}` : String(n));
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

export type TimeRank = 'year' | 'month' | 'day' | 'time';

/** An axis label for a tick of the given rank: "2027", "Dec", "18 Nov", "06:00" (UTC). */
export function formatAxisTime(t: number, rank: TimeRank): string {
  const d = new Date(t);
  switch (rank) {
    case 'year':
      return String(d.getUTCFullYear());
    case 'month':
      return MONTHS[d.getUTCMonth()];
    case 'day':
      return `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]}`;
    default:
      return `${two(d.getUTCHours())}:${two(d.getUTCMinutes())}`;
  }
}

/** The crosshair's time tag and the readout's time: "Tue 17 Nov 2026 03:45" (intraday) or "Tue 17 Nov 2026". */
export function formatCandleTime(t: number, intervalMs: number): string {
  const d = new Date(t);
  const day = `${DAYS[d.getUTCDay()]} ${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]} ${d.getUTCFullYear()}`;
  return intervalMs >= 86_400_000 ? day : `${day} ${two(d.getUTCHours())}:${two(d.getUTCMinutes())}`;
}

/** Words for an interval, for the accessible name: "15-minute", "4-hour", "1-day". */
export function intervalWords(intervalMs: number): string {
  if (intervalMs % 86_400_000 === 0) return `${intervalMs / 86_400_000}-day`;
  if (intervalMs % 3_600_000 === 0) return `${intervalMs / 3_600_000}-hour`;
  return `${Math.round(intervalMs / 60_000)}-minute`;
}
