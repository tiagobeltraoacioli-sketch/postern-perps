/**
 * The engine's own arithmetic, in the engine's units. UNAUDITED prototype.
 *
 * docs/SPEC.md 0.2.2 sections 1, 5, 6.2, 6.3, 7 and 9, transcribed formula by formula and pinned by the engine's
 * worked examples (engine/vectors/worked-examples.json, agent 430: 175 cases, 755 values, every one reproduced in
 * src/test/exact.test.ts). lib/calc.ts builds what the interface shows on top of these; nothing else in the page
 * does margin, fee, funding or liquidation arithmetic.
 *
 * Units (SPEC 1): price 1e-8 USD, size 1e-8 base, USDC 1e-6, notional = price x size = 1e-16 USD, margin and fee
 * rates in ppm, funding rate and premium 1e-9. Rounding (S-0.3) is stated at each function: what an account owes
 * rounds up, what it is owed rounds down.
 */
export const PPM = 1_000_000n;
export const NOTIONAL_PER_USDC = 10_000_000_000n;
export const RATE_SCALE = 1_000_000_000n;
export const MAX_PRICE = 1_000_000_000_000_000n;

const abs = (v: bigint) => (v < 0n ? -v : v);
/** floor(a / b) for b > 0 (toward minus infinity). */
export const floorDiv = (a: bigint, b: bigint) => (a >= 0n ? a / b : -((-a + b - 1n) / b));
/** ceil(a / b) for b > 0. */
export const ceilDiv = (a: bigint, b: bigint) => -floorDiv(-a, b);

/** A margin tier (SPEC 6.2): applies to the part of a position's notional at or above `floor` (USDC units). */
export type Tier = { floor: bigint; imfPpm: bigint; mmfPpm: bigint };

/** Notional of |size| at price, exact (notional units). */
export const notionalOf = (size: bigint, price: bigint) => abs(size) * price;
/** Notional units to USDC units, rounded up (a requirement, a fee) or down (a value owed to the account). */
export const usdcCeil = (notional: bigint) => ceilDiv(notional, NOTIONAL_PER_USDC);
export const usdcFloor = (notional: bigint) => floorDiv(notional, NOTIONAL_PER_USDC);

/** ceil(10^6 / leverage): leverage 3 is 333334 ppm, never 333333 (R395-15). */
export function leverageFractionPpm(leverage: number): bigint {
  const l = BigInt(Math.max(1, Math.floor(leverage)));
  return ceilDiv(PPM, l);
}

/**
 * The continuous piecewise requirement (SPEC 6.2): the sum over tiers of (the part of the notional inside the tier)
 * x fraction, then rounded UP to a notional unit. `floorPpm` raises every tier's fraction to at least that value
 * (the initial requirement's leverage fraction).
 */
function requirement(tiers: readonly Tier[], notional: bigint, pick: 'imfPpm' | 'mmfPpm', floorPpm = 0n): bigint {
  let sum = 0n;
  for (let i = 0; i < tiers.length; i++) {
    const from = tiers[i].floor * NOTIONAL_PER_USDC;
    if (notional <= from) break;
    const to = i + 1 < tiers.length ? tiers[i + 1].floor * NOTIONAL_PER_USDC : null;
    const part = (to !== null && notional > to ? to : notional) - from;
    const fraction = tiers[i][pick] > floorPpm ? tiers[i][pick] : floorPpm;
    sum += part * fraction;
  }
  return ceilDiv(sum, PPM);
}
/** Maintenance requirement of a position of this notional, in notional units (rounded up). */
export const maintenanceReq = (tiers: readonly Tier[], notional: bigint) => requirement(tiers, notional, 'mmfPpm');
/** Initial requirement at `leverage`: imf' = max(imf, ceil(10^6 / leverage)) per tier (rounded up). */
export const initialReq = (tiers: readonly Tier[], notional: bigint, leverage: number) => requirement(tiers, notional, 'imfPpm', leverageFractionPpm(leverage));

/** Trading fee in USDC units: ceil(notional x feePpm / 10^6 / 10^10). The payer pays at least the exact amount. */
export const feeOf = (notional: bigint, feePpm: bigint) => ceilDiv(notional * feePpm, PPM * NOTIONAL_PER_USDC);

/** Unrealised result of a position in notional units, exact: size x mark - openNotional. */
export const unrealisedOf = (size: bigint, mark: bigint, openNotional: bigint) => size * mark - openNotional;

export type CrossPosition = { size: bigint; mark: bigint; openNotional: bigint; tiers: readonly Tier[] };
/** Equity E (notional units): balance x 10^10 + the unrealised result of every cross position (SPEC 6.2). */
export function equityOf(balance: bigint, positions: readonly CrossPosition[]): bigint {
  let e = balance * NOTIONAL_PER_USDC;
  for (const p of positions) e += unrealisedOf(p.size, p.mark, p.openNotional);
  return e;
}
/** Maintenance MR (notional units): the sum of each position's requirement at its mark. */
export function maintenanceOf(positions: readonly CrossPosition[]): bigint {
  let mr = 0n;
  for (const p of positions) mr += maintenanceReq(p.tiers, notionalOf(p.size, p.mark));
  return mr;
}
/** The engine's test: liquidatable when E < MR (strict). */
export const liquidatable = (balance: bigint, positions: readonly CrossPosition[]) => equityOf(balance, positions) < maintenanceOf(positions);

/**
 * Liquidation price of position `index`: the mark of ITS market at which E < MR first holds with every other mark
 * held still. For a long the HIGHEST such mark, for a short the LOWEST; null when no price in [1, MAX_PRICE] does.
 * Found by exact integer search on the engine's own test (E - MR is monotonic in one position's mark because every
 * maintenance fraction is below 1), so it is the engine's answer to the unit, whatever the tiers.
 */
export function liquidationPriceOf(balance: bigint, positions: readonly CrossPosition[], index: number): bigint | null {
  const target = positions[index];
  if (!target || target.size === 0n) return null;
  const at = (mark: bigint) => liquidatable(balance, positions.map((p, i) => (i === index ? { ...p, mark } : p)));
  if (target.size > 0n) {
    // Liquidatable at low marks. Highest liquidatable mark.
    if (!at(1n)) return null;
    if (at(MAX_PRICE)) return MAX_PRICE;
    let lo = 1n; // liquidatable
    let hi = MAX_PRICE; // not liquidatable
    while (hi - lo > 1n) {
      const mid = (lo + hi) / 2n;
      if (at(mid)) lo = mid;
      else hi = mid;
    }
    return lo;
  }
  // A short is liquidatable at high marks. Lowest liquidatable mark.
  if (!at(MAX_PRICE)) return null;
  if (at(1n)) return 1n;
  let lo = 1n; // not liquidatable
  let hi = MAX_PRICE; // liquidatable
  while (hi - lo > 1n) {
    const mid = (lo + hi) / 2n;
    if (at(mid)) hi = mid;
    else lo = mid;
  }
  return hi;
}

const clamp = (v: bigint, lo: bigint, hi: bigint) => (v < lo ? lo : v > hi ? hi : v);

/**
 * Bankruptcy price (SPEC 6.3 step 2): the price at which E would be 0 holding the rest still; rounded toward the
 * account (up for a long, down for a short), clamped to [1, MAX_PRICE]. `rest` = balance x 10^10 + the unrealised
 * result of the OTHER positions, in notional units.
 */
export function bankruptcyPriceOf(size: bigint, openNotional: bigint, rest: bigint): bigint {
  if (size === 0n) throw new RangeError('no position');
  const exact = openNotional - rest;
  return clamp(size > 0n ? ceilDiv(exact, size) : floorDivSigned(exact, size), 1n, MAX_PRICE);
}
/** floor(a / b) for any non-zero b. */
function floorDivSigned(a: bigint, b: bigint): bigint {
  return b > 0n ? floorDiv(a, b) : floorDiv(-a, -b);
}
/** Liquidation price bound (R395-13): max(bp, mark - floor(mark x floor(mmf0 / 2) / 10^6)) for a long; min(bp, mark + ...) for a short. */
export function liquidationBoundOf(size: bigint, bankruptcy: bigint, mark: bigint, mmf0Ppm: bigint): bigint {
  const band = (mark * (mmf0Ppm / 2n)) / PPM;
  if (size > 0n) return bankruptcy > mark - band ? bankruptcy : mark - band;
  return bankruptcy < mark + band ? bankruptcy : mark + band;
}
/** Liquidation fee before the cap by remaining equity: liquidationFeePpm of the liquidated notional at mark, rounded up. */
export const liquidationFeeOf = (size: bigint, mark: bigint, feePpm: bigint) => feeOf(notionalOf(size, mark), feePpm);

/**
 * Funding rate of an interval (S-7.2): P = the lower-middle median of the premium samples;
 * rate = clamp(P + clamp(interest - P, -c, +c), -max, +max). No samples: null (no funding for that interval).
 */
export function fundingRateOf(samples: readonly bigint[], interest: bigint, fundingClamp: bigint, maxFundingRate: bigint): bigint | null {
  if (samples.length === 0) return null;
  const sorted = [...samples].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  const p = sorted[(sorted.length - 1) >> 1];
  return clamp(p + clamp(interest - p, -fundingClamp, fundingClamp), -maxFundingRate, maxFundingRate);
}
/**
 * Funding credited to a position for one interval, USDC units: floor(-(size x mark x rate) / 10^19). Negative: the
 * account pays (rounded up in size). A long pays when the rate is positive.
 */
export const fundingCreditOf = (size: bigint, mark: bigint, rate: bigint) => floorDiv(-(size * mark * rate), RATE_SCALE * NOTIONAL_PER_USDC);

/** Premium sample (S-7.1): trunc((max(0, impactBid - index) - max(0, index - impactAsk)) x 10^9 / index); a missing side counts as the index. */
export function premiumOf(index: bigint, impactBid: bigint | null, impactAsk: bigint | null): bigint {
  const bid = impactBid ?? index;
  const ask = impactAsk ?? index;
  const up = bid > index ? bid - index : 0n;
  const down = index > ask ? index - ask : 0n;
  return ((up - down) * RATE_SCALE) / index; // BigInt division truncates toward zero
}

/** Mark (SPEC 5): index + clamp(median(bid, ask, lastTrade) - index, -band, +band), band = floor(index x bps / 10^4); a missing value = the index; the result stays in [1, MAX_PRICE]. */
export function markOf(index: bigint, bestBid: bigint | null, bestAsk: bigint | null, freshLastTrade: bigint | null, markBandBps: bigint): bigint {
  const three = [bestBid ?? index, bestAsk ?? index, freshLastTrade ?? index].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  const band = (index * markBandBps) / 10_000n;
  return clamp(index + clamp(three[1] - index, -band, band), 1n, MAX_PRICE);
}
