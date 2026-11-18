/**
 * Every calculation the interface shows to a user: notional, margin, fees, PnL, liquidation price, funding, account
 * summary, market-order fill estimate, order preview. UNAUDITED prototype.
 *
 * Round 2: the arithmetic is the ENGINE's (lib/exact.ts: docs/SPEC.md 0.2.2 formula by formula, pinned by the
 * engine's 755 worked-example values). This file only adapts it to what the screens need: USDC amounts (requirements
 * and fees rounded up, values owed to the account rounded down, as docs/API.md 1 "Rounding of displayed values"),
 * prices put on the tick against the user, and the account-level sums. Round 1's own formulas (whole notional x tier
 * rate less a deduction; notional floored to USDC before the fee; a funding rate in 1e-8) are gone: they differed
 * from the engine by a unit in places, and the leverage fraction was not the engine's. Every test of round 1 is kept,
 * restated in these terms (src/test/lib.test.ts).
 *
 * A figure computed here is still labelled an estimate in the interface: it is exact for the inputs the page holds
 * (a position's open notional is taken as size x entry price, and the API's entry price is truncated to 1e-8), and
 * the engine's state is the truth.
 */
import { NOTIONAL_DIV, abs, max, min, mulDiv, toTick } from './fixed';
import * as E from './exact';

export type Side = 'long' | 'short';
export const dir = (side: Side) => (side === 'long' ? 1n : -1n);

/**
 * A margin tier as the exchange publishes it (docs/API.md `marginTiers`): it applies to the part of a position's
 * notional at or above `floor` (USDC units), up to the next tier's floor.
 */
export type Tier = E.Tier & { maxLeverage: number };

export type Fees = { takerPpm: bigint; makerPpm: bigint };
export const BASE_FEES: Fees = { takerPpm: 450n, makerPpm: 150n }; // 4.5 bps and 1.5 bps

const PPM = 1_000_000n;

/** Notional in USDC units of `size` (absolute, size scale) at `px`, for display (rounded as asked). */
export function notional(size: bigint, px: bigint, rounding: 'floor' | 'ceil' = 'floor'): bigint {
  return mulDiv(abs(size), px, NOTIONAL_DIV, rounding);
}

/** The tier that holds the LAST unit of a position of this notional (USDC units): its leverage ceiling applies. */
export function tierFor(tiers: Tier[], notionalUsdc: bigint): Tier {
  let found = tiers[0];
  for (const tier of tiers) if (notionalUsdc > tier.floor || tier === tiers[0]) found = tier;
  return found;
}

/** Maintenance margin (USDC, rounded up) of |size| at `px`: SPEC 6.2 continuous piecewise schedule. */
export function maintenanceMargin(tiers: Tier[], size: bigint, px: bigint): bigint {
  return E.usdcCeil(E.maintenanceReq(tiers, E.notionalOf(size, px)));
}

/** Highest leverage the tiers allow at this notional. */
export function maxLeverageAt(tiers: Tier[], notionalUsdc: bigint): number {
  return tierFor(tiers, notionalUsdc).maxLeverage;
}

/** Initial margin (USDC, rounded up) of |size| at `px` at `leverage`: each tier at max(imf, ceil(10^6 / leverage)). */
export function initialMargin(tiers: Tier[], size: bigint, px: bigint, leverage: number): bigint {
  return E.usdcCeil(E.initialReq(tiers, E.notionalOf(size, px), leverage));
}

/** Trading fee (USDC, rounded up) on a fill of |size| at `px`. */
export function fee(size: bigint, px: bigint, ratePpm: bigint): bigint {
  return E.feeOf(E.notionalOf(size, px), ratePpm);
}

/** Unrealised PnL in USDC units of a signed position (positive size is long), rounded down. */
export function pnl(size: bigint, entryPx: bigint, markPx: bigint): bigint {
  return E.usdcFloor(E.unrealisedOf(size, markPx, size * entryPx));
}

/** Return on the margin put up, in parts per million (PnL / margin). Null without margin. */
export function roePpm(pnlUsdc: bigint, marginUsdc: bigint): bigint | null {
  return marginUsdc > 0n ? mulDiv(pnlUsdc, PPM, marginUsdc, 'floor') : null;
}

/**
 * Funding paid (negative) or received (positive) by a signed position for one interval (S-7.3):
 * floor(-(size x mark x rate) / 10^19), `rate` in the engine's 1e-9. Longs pay when the rate is positive. What is
 * paid rounds up, what is received down.
 */
export function fundingPayment(size: bigint, markPx: bigint, rate: bigint): bigint {
  return E.fundingCreditOf(size, markPx, rate);
}

/**
 * Liquidation price of ONE position backed by `backing` USDC alone (an isolated position's margin, or a cross
 * account with nothing else in it): the mark at which the engine's test equity < maintenance first holds, found by
 * exact search (lib/exact.ts), then put on the tick against the user (up for a long, down for a short). Null when
 * no price liquidates it.
 */
export function liquidationPrice(args: { size: bigint; entryPx: bigint; backing: bigint; tiers: Tier[]; tick: bigint }): bigint | null {
  const { size, entryPx, backing, tiers, tick } = args;
  if (size === 0n) return null;
  const p = E.liquidationPriceOf(backing, [{ size, mark: entryPx, openNotional: size * entryPx, tiers }], 0);
  return p === null ? null : toTick(p, tick, size > 0n ? 'ceil' : 'floor');
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
  /** Signed open notional (notional units) when the page holds it exactly; otherwise size x entryPx is used. */
  openNotional?: bigint;
};
const openOf = (p: PositionInput) => p.openNotional ?? p.size * p.entryPx;
const cross = (p: PositionInput): E.CrossPosition => ({ size: p.size, mark: p.markPx, openNotional: openOf(p), tiers: p.tiers });

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
  // Sums are kept in notional units and rounded once (requirements up, values down), as the engine and the API do.
  let unrealisedN = 0n;
  let crossUnrealisedN = 0n;
  let maintenanceN = 0n;
  let crossMaintenanceN = 0n;
  let crossInitialN = 0n;
  let isolatedMargins = 0n;
  for (const p of positions) {
    const u = E.unrealisedOf(p.size, p.markPx, openOf(p));
    const n = E.notionalOf(p.size, p.markPx);
    const mm = E.maintenanceReq(p.tiers, n);
    unrealisedN += u;
    maintenanceN += mm;
    if (p.isolated) isolatedMargins += p.isolatedMargin;
    else {
      crossUnrealisedN += u;
      crossMaintenanceN += mm;
      crossInitialN += E.initialReq(p.tiers, n, p.leverage);
    }
  }
  const unrealised = E.usdcFloor(unrealisedN);
  const equity = E.usdcFloor(collateral * E.NOTIONAL_PER_USDC + unrealisedN);
  const maintenance = E.usdcCeil(maintenanceN);
  const crossEquity = E.usdcFloor((collateral - isolatedMargins) * E.NOTIONAL_PER_USDC + crossUnrealisedN);
  const crossInitial = E.usdcCeil(crossInitialN) + orderMargin;
  return {
    equity,
    unrealised,
    marginUsed: crossInitial + isolatedMargins,
    maintenance,
    marginRatioPpm: maintenance > 0n && equity > 0n ? mulDiv(maintenance, PPM, equity, 'ceil') : maintenance > 0n ? PPM : null,
    available: max(0n, crossEquity - crossInitial),
    crossEquity,
    crossMaintenance: E.usdcCeil(crossMaintenanceN),
  };
}

/**
 * Liquidation price of position `index` inside an account (the other marks held still): the engine's own test by
 * exact search, on the tick against the user. A cross position is tested with every cross position of the account
 * and the cross balance; an isolated one alone with its margin (SPEC 6.2 last bullet).
 */
export function positionLiquidationPrice(collateral: bigint, positions: PositionInput[], index: number): bigint | null {
  const p = positions[index];
  if (p.isolated) return liquidationPrice({ size: p.size, entryPx: p.entryPx, backing: p.isolatedMargin, tiers: p.tiers, tick: p.tick });
  let balance = collateral;
  const crossed: E.CrossPosition[] = [];
  let at = -1;
  positions.forEach((o, i) => {
    if (o.isolated) balance -= o.isolatedMargin;
    else {
      if (i === index) at = crossed.length;
      crossed.push(cross(o));
    }
  });
  const exact = E.liquidationPriceOf(balance, crossed, at);
  return exact === null ? null : toTick(exact, p.tick, p.size > 0n ? 'ceil' : 'floor');
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

/**
 * Largest order size (size scale, multiple of `step`) whose initial margin plus taker fee fits in `budget` USDC,
 * with the engine's own margin and fee (so the answer never exceeds the budget by a rounding unit).
 */
export function sizeForBudget(args: { budget: bigint; px: bigint; leverage: number; feePpm: bigint; step: bigint; tiers: Tier[] }): bigint {
  const { budget, px, leverage, feePpm, step, tiers } = args;
  if (budget <= 0n || px <= 0n || step <= 0n) return 0n;
  const cost = (size: bigint) => initialMargin(tiers, size, px, leverage) + fee(size, px, feePpm);
  // First guess from the leverage fraction alone (budget >= n x f + n x fee), then step down while the exact cost
  // (tiers can ask for more than the leverage fraction) is above the budget. The cost grows with size, so the first
  // size that fits is the largest.
  const fraction = E.leverageFractionPpm(leverage);
  const n = mulDiv(budget, PPM, fraction + feePpm, 'floor');
  let size = mulDiv(n, NOTIONAL_DIV, px, 'floor');
  size -= size % step;
  if (size > 0n && cost(size) > budget) {
    // Binary search for the largest multiple of the step that fits.
    let lo = 0n;
    let hi = size / step;
    while (hi - lo > 1n) {
      const mid = (lo + hi) / 2n;
      if (cost(mid * step) <= budget) lo = mid;
      else hi = mid;
    }
    size = lo * step;
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
  const f = fee(size, px, taker ? fees.takerPpm : fees.makerPpm);
  const increases = abs(after.size) > abs(before.size);
  const afterN = notional(after.size, markPx);
  // Requirements are measured at the mark (SPEC 6.2), the engine's own schedule for both sizes.
  const margin = increases ? max(0n, initialMargin(tiers, after.size, markPx, leverage) - (current ? initialMargin(current.tiers, before.size, markPx, current.leverage) : 0n)) : 0n;
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
