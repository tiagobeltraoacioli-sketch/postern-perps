/** Wire (decimal strings, docs/API.md) to fixed-point integers. The only place where API text becomes numbers. */
import { PX_DECIMALS, RATE_DECIMALS, SZ_DECIMALS, USDC_DECIMALS, parseFixedLoose as p, toNumber } from './fixed';
import type * as T from './types';

const px = (s: string) => p(s, PX_DECIMALS);
const sz = (s: string) => p(s, SZ_DECIMALS);
const usd = (s: string) => p(s, USDC_DECIMALS);
/** A fraction ("0.01") or basis points ("4.5") as parts per million. */
const fractionPpm = (s: string) => p(s, 6);
const bpsPpm = (s: string) => p(s, 2);

export function market(w: T.WireMarket): T.Market {
  // docs/API.md `marginTiers`: each tier applies from its `notionalFloor`; fractions are exact decimals in ppm.
  const tiers = w.marginTiers.map((tier) => ({
    floor: usd(tier.notionalFloor),
    imfPpm: fractionPpm(tier.initialMarginFraction),
    mmfPpm: fractionPpm(tier.maintenanceMarginFraction),
    maxLeverage: Number(tier.maxLeverage),
  }));
  return {
    symbol: w.symbol,
    marketId: w.marketId,
    base: w.base,
    status: w.status,
    haltReason: w.haltReason ?? null,
    priceDecimals: w.priceDecimals,
    sizeDecimals: w.sizeDecimals,
    tick: px(w.tickSize),
    step: sz(w.stepSize),
    minSize: sz(w.minOrderSize),
    maxSize: sz(w.maxOrderSize),
    maxLeverage: Number(w.maxLeverage),
    tiers,
    makerPpm: bpsPpm(w.makerFeeBps),
    takerPpm: bpsPpm(w.takerFeeBps),
    liquidationFeePpm: bpsPpm(w.liquidationFeeBps),
    fundingIntervalSec: w.fundingIntervalSec,
    maxFundingRate: p(w.maxFundingRate, RATE_DECIMALS),
    oracle: w.oracle,
    marketSlippagePpm: typeof w.marketSlippageBps === 'number' && Number.isInteger(w.marketSlippageBps) && w.marketSlippageBps > 0 && w.marketSlippageBps <= 10_000 ? BigInt(w.marketSlippageBps) * 100n : null,
    openInterestCap: sz(w.openInterestCap),
  };
}

export function ticker(w: T.WireTicker): T.Ticker {
  return {
    symbol: w.symbol,
    mark: px(w.markPrice),
    index: px(w.indexPrice),
    last: px(w.lastPrice),
    fundingRate: p(w.fundingRate, RATE_DECIMALS),
    nextFundingTime: w.nextFundingTime,
    openInterest: sz(w.openInterest),
    volume24h: usd(w.volume24h),
    high24h: px(w.high24h),
    low24h: px(w.low24h),
    changePpm: fractionPpm(w.change24h),
    time: w.time,
  };
}

export const level = ([price, size]: [string, string]) => ({ px: px(price), size: sz(size) });

export function trade(w: T.WireTrade): T.Trade {
  return { id: w.tradeId, px: px(w.price), size: sz(w.size), buy: w.side === 'buy', time: w.time, liquidation: w.liquidation };
}

/** Candles are geometry: floats are fine here and only here. */
export function candle(w: T.WireCandle): T.Candle {
  return { t: w.t, o: Number(w.o), h: Number(w.h), l: Number(w.l), c: Number(w.c), v: Number(w.v) };
}

/**
 * The node's exact open notional (F434-4), or null. It is taken only when it is a plain signed integer whose sign is
 * the position's and which lies within one part in a thousand of size x entry price (the entry price on the wire is
 * that notional divided by the size, rounded to the market's decimals): a figure that fails this is not trusted and
 * the page falls back to size x entry price, as before the field existed.
 */
export function openNotionalOf(raw: unknown, size: bigint, entryPx: bigint): bigint | null {
  if (typeof raw !== 'string' || !/^-?\d{1,60}$/.test(raw)) return null;
  const v = BigInt(raw);
  const approx = size * entryPx;
  if (v === 0n || approx === 0n || v > 0n !== approx > 0n) return null;
  const diff = v > approx ? v - approx : approx - v;
  const bound = (approx < 0n ? -approx : approx) / 1000n;
  return diff <= bound ? v : null;
}

export function position(w: T.WirePosition): T.Position {
  return {
    symbol: w.symbol,
    size: sz(w.size),
    entryPx: px(w.entryPrice),
    openNotional: openNotionalOf(w.openNotional, sz(w.size), px(w.entryPrice)),
    markPx: px(w.markPrice),
    notional: usd(w.notional),
    unrealised: usd(w.unrealizedPnl),
    realised: usd(w.realizedPnl),
    liquidationPx: w.liquidationPrice === null ? null : px(w.liquidationPrice),
    isolated: w.marginMode === 'isolated',
    leverage: Number(w.leverage),
    isolatedMargin: w.isolatedMargin === null ? 0n : usd(w.isolatedMargin),
    initialMargin: usd(w.initialMargin),
    maintenanceMargin: usd(w.maintenanceMargin),
    funding: usd(w.fundingSinceOpen),
  };
}

export function account(w: T.WireAccount): T.Account {
  const positions = w.positions.map(position);
  return {
    address: w.address.toLowerCase(),
    height: w.height,
    time: w.time,
    collateral: usd(w.collateral),
    unrealised: usd(w.unrealizedPnl),
    equity: usd(w.equity),
    initialMargin: usd(w.initialMargin),
    maintenanceMargin: usd(w.maintenanceMargin),
    free: usd(w.freeCollateral),
    withdrawable: usd(w.withdrawable),
    marginRatioPpm: positions.length ? fractionPpm(w.marginRatio) : null,
    feeTier: { tier: w.feeTier.tier, makerPpm: bpsPpm(w.feeTier.makerFeeBps), takerPpm: bpsPpm(w.feeTier.takerFeeBps), volume30d: usd(w.feeTier.volume30d) },
    positions,
  };
}

export function order(w: T.WireOrder): T.Order {
  return {
    id: w.orderId,
    symbol: w.symbol,
    buy: w.side === 'buy',
    type: w.type,
    tif: w.timeInForce,
    px: w.price === null || w.price === '0' ? null : px(w.price),
    size: sz(w.size),
    filled: sz(w.filledSize),
    avgPx: w.avgFillPrice === null ? null : px(w.avgFillPrice),
    reduceOnly: w.reduceOnly,
    triggerPx: w.triggerPrice === null || w.triggerPrice === '0' ? null : px(w.triggerPrice),
    triggerCondition: w.triggerCondition,
    status: w.status,
    createdAt: w.createdAt,
    updatedAt: w.updatedAt,
    rejectReason: w.rejectReason,
  };
}

export function fill(w: T.WireFill): T.Fill {
  return {
    id: w.fillId,
    orderId: w.orderId,
    symbol: w.symbol,
    buy: w.side === 'buy',
    px: px(w.price),
    size: sz(w.size),
    fee: usd(w.fee),
    maker: w.liquidity === 'maker',
    realised: usd(w.realizedPnl),
    liquidation: w.liquidation,
    time: w.time,
  };
}

export function fundingPayment(w: T.WireFundingPayment): T.FundingPayment {
  return { symbol: w.symbol, time: w.time, rate: p(w.rate, RATE_DECIMALS), size: sz(w.positionSize), markPx: px(w.markPrice), payment: usd(w.payment) };
}
export function deposit(w: T.WireDeposit): T.Deposit {
  return { id: w.depositId, amount: usd(w.amount), txHash: w.l2TxHash, l2Block: w.l2Block, status: w.status, time: w.time };
}
export function withdrawal(w: T.WireWithdrawal): T.Withdrawal {
  return { id: w.withdrawalId, recipient: w.recipient, amount: usd(w.amount), fee: usd(w.fee), status: w.status, txHash: w.l2TxHash, availableAt: w.availableAt, time: w.time, certificate: w.certificate ?? null };
}

export const pxNumber = (v: bigint) => toNumber(v, PX_DECIMALS);
export const szNumber = (v: bigint) => toNumber(v, SZ_DECIMALS);
