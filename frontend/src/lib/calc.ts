/**
 * Every calculation the interface shows to a user: notional, margin, fees, PnL, liquidation price, funding, account
 * summary, market-order fill estimate, order preview. Integer arithmetic only, rounding stated at each step
 * (requirements round up, what the user receives rounds down), mirroring the engine's rules.
 *
 * UNAUDITED prototype. The formulas are the brief's (USDC cross margin, isolated as an option, margin tiers by
 * position notional, maker/taker fees, hourly funding). They are a frontend fixture until docs/SPEC.md (agent 350)
 * publishes the canonical rules and worked examples; src/test/calc.test.ts holds the worked examples used here.
 * A figure computed here is always labelled an estimate in the interface: the engine's state is the truth.
 */
import { NOTIONAL_DIV, RATE, abs, max, min, mulDiv, toTick } from './fixed';

export type Side = 'long' | 'short';
export const dir = (side: Side) => (side === 'long' ? 1n : -1n);

/** A margin tier: applies while the position notional is at most `upTo` (USDC units; the last tier has null). */
export type Tier = {
  upTo: bigint | null;
  maxLeverage: number;
  /** Maintenance margin rate in parts per million of notional. */
  mmrPpm: bigint;
  /** Maintenance deduction (USDC units) that makes the requirement continuous across tiers. */
  deduction: bigint;
};

export type Fees = { takerPpm: bigint; makerPpm: bigint };
export const BASE_FEES: Fees = { takerPpm: 450n, makerPpm: 150n }; // 4.5 bps and 1.5 bps

const PPM = 1_000_000n;

/** Notional in USDC units of `size` (absolute, size scale) at `px`. */
export function notional(size: bigint, px: bigint, rounding: 'floor' | 'ceil' = 'floor'): bigint {
  return mulDiv(abs(size), px, NOTIONAL_DIV, rounding);
}

export function tierFor(tiers: Tier[], notionalUsdc: bigint): Tier {
  for (const tier of tiers) if (tier.upTo === null || notionalUsdc <= tier.upTo) return tier;
  return tiers[tiers.length - 1];
}

/** Builds continuous deductions for a tier table given as (upTo, maxLeverage, mmrPpm). */
export function withDeductions(rows: { upTo: bigint | null; maxLeverage: number; mmrPpm: bigint }[]): Tier[] {
  const tiers: Tier[] = [];
  let deduction = 0n;
  let previous: { upTo: bigint | null; mmrPpm: bigint } | null = null;
  for (const row of rows) {
    if (previous && previous.upTo !== null) deduction += mulDiv(previous.upTo, row.mmrPpm - previous.mmrPpm, PPM, 'floor');
    tiers.push({ ...row, deduction });
    previous = row;
  }
  return tiers;
}

/** Maintenance margin of a position of this notional: notional x rate, rounded up, less the tier's deduction. */
export function maintenanceMargin(tiers: Tier[], notionalUsdc: bigint): bigint {
  const tier = tierFor(tiers, notionalUsdc);
  return max(0n, mulDiv(notionalUsdc, tier.mmrPpm, PPM, 'ceil') - tier.deduction);
}

/** Highest leverage the tiers allow at this notional. */
export function maxLeverageAt(tiers: Tier[], notionalUsdc: bigint): number {
  return tierFor(tiers, notionalUsdc).maxLeverage;
}

/** Initial margin: notional / leverage, rounded up. */
export function initialMargin(notionalUsdc: bigint, leverage: number): bigint {
  return mulDiv(notionalUsdc, 1n, BigInt(Math.max(1, Math.floor(leverage))), 'ceil');
}

/** Trading fee on a fill of this notional, rounded up. */
export function fee(notionalUsdc: bigint, ratePpm: bigint): bigint {
  return mulDiv(notionalUsdc, ratePpm, PPM, 'ceil');
}

/** Unrealised PnL in USDC units of a signed position (positive size is long), rounded down. */
export function pnl(size: bigint, entryPx: bigint, markPx: bigint): bigint {
  return mulDiv(size, markPx - entryPx, NOTIONAL_DIV, 'floor');
}

/** Return on the margin put up, in parts per million (PnL / margin). Null without margin. */
export function roePpm(pnlUsdc: bigint, marginUsdc: bigint): bigint | null {
  return marginUsdc > 0n ? mulDiv(pnlUsdc, PPM, marginUsdc, 'floor') : null;
}

/**
 * Funding paid (negative) or received (positive) by a signed position for one interval:
 * -(size x oraclePrice x rate). Longs pay when the rate is positive. What is paid rounds up, what is received down.
 */
export function fundingPayment(size: bigint, oraclePx: bigint, rate: bigint): bigint {
  return mulDiv(-size * oraclePx, rate, NOTIONAL_DIV * RATE, 'floor');
}

/**
 * Liquidation price of ONE position, everything else in the account held constant.
 *
 * The position is liquidated when the margin that backs it no longer covers its maintenance margin:
 *   backing + size x (p - entry) = |size| x p x mmr - deduction
 * `backing` is the isolated margin of an isolated position; for a cross position it is the account's collateral
 * plus the unrealised PnL of the other cross positions less their maintenance margin.
 * The tier is the one whose notional range contains the answer. Rounded to the tick against the user
 * (up for a long, down for a short). Null when no positive price liquidates the position.
 */
export function liquidationPrice(args: {
  size: bigint; // signed
  entryPx: bigint;
  backing: bigint; // USDC units
  tiers: Tier[];
  tick: bigint;
}): bigint | null {
  const { size, entryPx, backing, tiers, tick } = args;
  if (size === 0n) return null;
  const q = abs(size);
  const long = size > 0n;
  let lower = 0n;
  for (const tier of tiers) {
    // USDC-unit equation scaled back to price units: multiply USDC amounts by NOTIONAL_DIV.
    const fixedTerm = (backing + tier.deduction) * NOTIONAL_DIV;
    const numerator = long ? q * entryPx - fixedTerm : q * entryPx + fixedTerm;
    const denominator = long ? q * (PPM - tier.mmrPpm) : q * (PPM + tier.mmrPpm);
    if (numerator > 0n && denominator > 0n) {
      const p = mulDiv(numerator, PPM, denominator, long ? 'ceil' : 'floor');
      const n = notional(q, p);
      const inTier = n > lower - 1n && (tier.upTo === null || n <= tier.upTo);
      if (inTier || tier.upTo === null) return toTick(p, tick, long ? 'ceil' : 'floor');
    } else if (tier.upTo === null) return null;
    lower = tier.upTo === null ? lower : tier.upTo + 1n;
  }
  return null;
}

export type PositionInput = {
  market: string;
  size: bigint; // signed
  entryPx: bigint;
  markPx: bigint;
  leverage: number;
  isolated: boolean;
  isolatedMargin: bigint; // USDC units, zero for cross
  tiers: Tier[];
  tick: bigint;
};

export type AccountSummary = {
  /** Collateral plus all unrealised PnL (isolated margins are part of collateral here). */
  equity: bigint;
  unrealised: bigint;
  /** Initial margin held by positions and resting orders. */
  marginUsed: bigint;
  maintenance: bigint;
  /** maintenance / equity in parts per million; liquidation of cross positions begins at 1_000_000. Null when no position. */
  marginRatioPpm: bigint | null;
  /** What a new order can use: cross equity less margin in use, never negative. */
  available: bigint;
  /** Cross backing: collateral not in isolated positions, plus cross PnL. */
  crossEquity: bigint;
  crossMaintenance: bigint;
};

export function accountSummary(collateral: bigint, positions: PositionInput[], orderMargin = 0n): AccountSummary {
  let unrealised = 0n;
  let marginUsed = orderMargin;
  let maintenance = 0n;
  let crossEquity = collateral;
  let crossMaintenance = 0n;
  let crossInitial = orderMargin;
  for (const p of positions) {
    const upnl = pnl(p.size, p.entryPx, p.markPx);
    const n = notional(p.size, p.markPx);
    const mm = maintenanceMargin(p.tiers, n);
    unrealised += upnl;
    maintenance += mm;
    if (p.isolated) {
      marginUsed += p.isolatedMargin;
      crossEquity -= p.isolatedMargin;
    } else {
      const im = initialMargin(n, p.leverage);
      marginUsed += im;
      crossInitial += im;
      crossEquity += upnl;
      crossMaintenance += mm;
    }
  }
  const equity = collateral + unrealised;
  return {
    equity,
    unrealised,
    marginUsed,
    maintenance,
    marginRatioPpm: maintenance > 0n && equity > 0n ? mulDiv(maintenance, PPM, equity, 'ceil') : maintenance > 0n ? PPM : null,
    available: max(0n, crossEquity - crossInitial),
    crossEquity,
    crossMaintenance,
  };
}

/** Liquidation price of position `index` inside an account (the others held constant). */
export function positionLiquidationPrice(collateral: bigint, positions: PositionInput[], index: number): bigint | null {
  const p = positions[index];
  if (p.isolated) return liquidationPrice({ size: p.size, entryPx: p.entryPx, backing: p.isolatedMargin, tiers: p.tiers, tick: p.tick });
  let backing = collateral;
  positions.forEach((o, i) => {
    if (o.isolated) backing -= o.isolatedMargin;
    else if (i !== index) backing += pnl(o.size, o.entryPx, o.markPx) - maintenanceMargin(o.tiers, notional(o.size, o.markPx));
  });
  return liquidationPrice({ size: p.size, entryPx: p.entryPx, backing, tiers: p.tiers, tick: p.tick });
}

export type Level = { px: bigint; size: bigint };

/**
 * Walks the opposite side of the book for a market order. `levels` are best first.
 * Returns what would fill now, the average and worst price, and the slippage of the average against the best
 * price in parts per million (always non-negative).
 */
export function estimateMarketFill(levels: Level[], size: bigint): {
  filled: bigint;
  avgPx: bigint | null;
  worstPx: bigint | null;
  slippagePpm: bigint | null;
  complete: boolean;
} {
  let left = abs(size);
  let cost = 0n; // sum(px x size)
  let worst: bigint | null = null;
  for (const level of levels) {
    if (left === 0n) break;
    const take = min(left, level.size);
    if (take <= 0n) continue;
    cost += take * level.px;
    left -= take;
    worst = level.px;
  }
  const filled = abs(size) - left;
  if (filled === 0n || levels.length === 0) return { filled: 0n, avgPx: null, worstPx: null, slippagePpm: null, complete: false };
  const avgPx = cost / filled;
  const best = levels[0].px;
  return { filled, avgPx, worstPx: worst, slippagePpm: mulDiv(abs(avgPx - best), PPM, best, 'ceil'), complete: left === 0n };
}

/** The position after a fill of `fillSize` (signed: positive buys) at `fillPx`. Realised PnL in USDC units. */
export function applyFill(position: { size: bigint; entryPx: bigint }, fillSize: bigint, fillPx: bigint): { size: bigint; entryPx: bigint; realised: bigint } {
  const { size, entryPx } = position;
  if (size === 0n || size > 0n === fillSize > 0n) {
    const total = size + fillSize;
    // Size-weighted average entry, rounded against the user (up for a long, down for a short).
    const entry = total === 0n ? 0n : mulDiv(abs(size) * entryPx + abs(fillSize) * fillPx, 1n, abs(total), total > 0n ? 'ceil' : 'floor');
    return { size: total, entryPx: entry, realised: 0n };
  }
  const closing = min(abs(fillSize), abs(size));
  const realised = pnl(size > 0n ? closing : -closing, entryPx, fillPx);
  const total = size + fillSize;
  if (total === 0n) return { size: 0n, entryPx: 0n, realised };
  // Reduced: entry unchanged. Flipped: the remainder opens at the fill price.
  return { size: total, entryPx: total > 0n === size > 0n ? entryPx : fillPx, realised };
}

/** Largest order size (size scale, multiple of `step`) whose margin plus taker fee fits in `budget` USDC. */
export function sizeForBudget(args: { budget: bigint; px: bigint; leverage: number; feePpm: bigint; step: bigint }): bigint {
  const { budget, px, leverage, feePpm, step } = args;
  if (budget <= 0n || px <= 0n) return 0n;
  const lev = BigInt(Math.max(1, Math.floor(leverage)));
  // budget >= n/lev + n x fee  =>  n <= budget x lev x PPM / (PPM + lev x fee)
  const n = mulDiv(budget * lev, PPM, PPM + lev * feePpm, 'floor');
  let size = mulDiv(n, NOTIONAL_DIV, px, 'floor');
  size -= size % step;
  // Rounding up of margin and fee can exceed the budget by a unit: step down until it fits.
  while (size > 0n) {
    const nn = notional(size, px, 'ceil');
    if (initialMargin(nn, leverage) + fee(nn, feePpm) <= budget) break;
    size -= step;
  }
  return max(0n, size);
}

export type OrderPreviewInput = {
  side: Side;
  size: bigint; // absolute
  /** Limit price, or for a market order the estimated average fill price. */
  px: bigint;
  /** Mark price of the market (margin and liquidation are measured at mark). */
  markPx: bigint;
  leverage: number;
  isolated: boolean;
  reduceOnly: boolean;
  taker: boolean;
  fees: Fees;
  tiers: Tier[];
  tick: bigint;
  /** Account collateral in USDC units and its positions; `index` of this market's position or -1. */
  collateral: bigint;
  positions: PositionInput[];
  index: number;
  orderMargin: bigint;
};

export type OrderPreview = {
  notional: bigint;
  /** Fee if the whole order fills at `px` as taker or maker. */
  fee: bigint;
  /** Extra initial margin the order needs (zero when it only reduces). */
  margin: bigint;
  /** margin + fee. */
  cost: bigint;
  after: { size: bigint; entryPx: bigint; realised: bigint };
  /** Estimated liquidation price of the position after the fill; null when flat or never. */
  liquidationPx: bigint | null;
  /** Leverage tier ceiling at the resulting notional. */
  maxLeverage: number;
  problems: ('insufficient-margin' | 'leverage-above-tier' | 'reduce-only-increases' | 'would-liquidate')[];
};

export function orderPreview(input: OrderPreviewInput): OrderPreview {
  const { side, size, px, markPx, leverage, isolated, taker, fees, tiers, tick, collateral, positions, index } = input;
  const current = index >= 0 ? positions[index] : null;
  const before = { size: current?.size ?? 0n, entryPx: current?.entryPx ?? 0n };
  const after = applyFill(before, dir(side) * size, px);
  const n = notional(size, px, 'ceil');
  const f = fee(n, taker ? fees.takerPpm : fees.makerPpm);
  const increases = abs(after.size) > abs(before.size);
  const beforeN = notional(before.size, markPx);
  const afterN = notional(after.size, markPx);
  const margin = increases ? max(0n, initialMargin(afterN, leverage) - (current ? initialMargin(beforeN, current.leverage) : 0n)) : 0n;
  const problems: OrderPreview['problems'] = [];
  const summary = accountSummary(collateral, positions, input.orderMargin);
  if (input.reduceOnly && (increases || before.size === 0n || after.size > 0n !== before.size > 0n && after.size !== 0n)) problems.push('reduce-only-increases');
  if (increases && margin + f > summary.available) problems.push('insufficient-margin');
  const tierMax = maxLeverageAt(tiers, afterN);
  if (increases && leverage > tierMax) problems.push('leverage-above-tier');

  // The account after the fill: fee and realised PnL settle into collateral.
  const collateralAfter = collateral - f + after.realised;
  const next: PositionInput[] = positions.map((p) => ({ ...p }));
  let at = index;
  if (after.size === 0n) {
    if (at >= 0) next.splice(at, 1);
    at = -1;
  } else {
    const isolatedMargin = isolated
      ? current && current.isolated
        ? increases
          ? current.isolatedMargin + margin
          : mulDiv(current.isolatedMargin, abs(after.size), abs(before.size), 'floor')
        : margin
      : 0n;
    const p: PositionInput = {
      market: current?.market ?? '',
      size: after.size,
      entryPx: after.entryPx,
      markPx,
      leverage,
      isolated,
      isolatedMargin,
      tiers,
      tick,
    };
    if (at >= 0) next[at] = p;
    else at = next.push(p) - 1;
  }
  const liquidationPx = at >= 0 ? positionLiquidationPrice(collateralAfter, next, at) : null;
  if (liquidationPx !== null && after.size !== 0n && (after.size > 0n ? liquidationPx >= markPx : liquidationPx <= markPx)) problems.push('would-liquidate');
  return { notional: n, fee: f, margin, cost: margin + f, after, liquidationPx, maxLeverage: tierMax, problems };
}

/** Margin ratio as a health word. Thresholds are presentation only. */
export function health(marginRatioPpm: bigint | null): 'none' | 'safe' | 'caution' | 'danger' {
  if (marginRatioPpm === null) return 'none';
  if (marginRatioPpm >= 800_000n) return 'danger';
  if (marginRatioPpm >= 500_000n) return 'caution';
  return 'safe';
}
