/**
 * From the order form to a PlaceOrder message. Pure; UNAUDITED prototype.
 *
 * Rules (each tested in src/test/order-builder.test.ts):
 *  - Typed text is parsed at the market's own decimals; more places than the market has is an error, never a
 *    silent rounding. A price must be on the tick and a size on the step: the form says which, it does not round.
 *  - A market order (and a stop-market or take-profit-market order) always carries a WORST PRICE: the mark price
 *    (or the trigger price for a triggered order) moved by the user's slippage bound, rounded to the tick toward the
 *    user's protection (a buy bound down, a sell bound up). With one sequencer ordering the book, price 0 ("any
 *    price") would be a blank cheque, so it is never built.
 *  - The bound is anchored to the mark (oracle index plus a bounded book component), not to the visible book, so a
 *    thin or spoofed book cannot widen it.
 *  - Market orders are immediate-or-cancel; post-only exists only for limit orders.
 *  - A trigger already crossed by the mark is reported (it would trigger at once).
 */
import { PX_DECIMALS, SZ_DECIMALS, mulDiv, parseFixed, toDecimalString, toTick } from '../lib/fixed';
import { ORDER_TYPES, TIFS, triggerCondition, type Message } from '../lib/eip712';
import type { Market, OrderType, TimeInForce } from '../lib/types';
import { estimateMarketFill, type Level } from '../lib/calc';

export type OrderForm = {
  side: 'buy' | 'sell';
  kind: OrderType;
  size: string;
  price: string;
  trigger: string;
  tif: TimeInForce;
  reduceOnly: boolean;
  /** Slippage bound of market orders, parts per million (10_000 = 1%). */
  slippagePpm: bigint;
};

export type BuiltOrder = {
  message: Message | null;
  errors: string[];
  warnings: string[];
  size: bigint | null;
  /** Limit price, or the worst price of a market order. */
  price: bigint | null;
  trigger: bigint | null;
  isMarket: boolean;
  /** Market orders: what the visible book fills now within the bound. */
  estimate: ReturnType<typeof estimateMarketFill> | null;
};

export const MAX_SLIPPAGE_PPM = 100_000n; // 10%: wider than this is refused by the form
export const DEFAULT_SLIPPAGE_PPM = 10_000n; // 1%

const PPM = 1_000_000n;
const scale = (v: bigint, from: number, to: number) => v * 10n ** BigInt(to - from);

/** Parses `text` at `marketDecimals` places into the engine scale `engineDecimals`. Null when empty or invalid. */
export function parseAt(text: string, marketDecimals: number, engineDecimals: number): { value: bigint | null; error: string | null } {
  const t = text.trim().replace(/,/g, '');
  if (t === '') return { value: null, error: null };
  if (t.startsWith('-')) return { value: null, error: 'must be positive' };
  const v = parseFixed(t, marketDecimals);
  if (v === null) return { value: null, error: /\.\d*$/.test(t) && /^\d*\.?\d*$/.test(t) ? `at most ${marketDecimals} decimal places` : 'is not a number' };
  return { value: scale(v, marketDecimals, engineDecimals), error: null };
}

export const isMarketKind = (k: OrderType) => k === 'market' || k === 'stopMarket' || k === 'takeProfitMarket';
export const isTriggerKind = (k: OrderType) => k !== 'limit' && k !== 'market';

/** The worst price of a market order at `reference`, rounded to the tick in the user's favour. */
export function worstPrice(reference: bigint, buy: boolean, slippagePpm: bigint, tick: bigint): bigint {
  const raw = mulDiv(reference, PPM + (buy ? slippagePpm : -slippagePpm), PPM, buy ? 'floor' : 'ceil');
  return toTick(raw, tick, buy ? 'floor' : 'ceil');
}

export function buildOrder(args: {
  form: OrderForm;
  market: Market;
  mark: bigint | null;
  /** Opposite side of the book, best first (asks for a buy). */
  levels: Level[];
  account: string | null;
  nonce: bigint | null;
  clientOrderId: string;
}): BuiltOrder {
  const { form, market, mark, levels, account, nonce, clientOrderId } = args;
  const errors: string[] = [];
  const warnings: string[] = [];
  const buy = form.side === 'buy';
  const isMarket = isMarketKind(form.kind);
  const triggered = isTriggerKind(form.kind);

  const s = parseAt(form.size, market.sizeDecimals, SZ_DECIMALS);
  if (s.error) errors.push(`Size ${s.error}.`);
  const size = s.value;
  if (size !== null) {
    if (size === 0n) errors.push('Size must be above zero.');
    else if (size % market.step !== 0n) errors.push(`Size must be a multiple of ${formatStep(market)}.`);
    else if (size < market.minSize) errors.push(`The smallest order is ${trim(market.minSize, SZ_DECIMALS, market.sizeDecimals)} ${market.base}.`);
    else if (size > market.maxSize) errors.push(`The largest order is ${trim(market.maxSize, SZ_DECIMALS, market.sizeDecimals)} ${market.base}.`);
  }

  let trigger: bigint | null = null;
  if (triggered) {
    const t = parseAt(form.trigger, market.priceDecimals, PX_DECIMALS);
    if (t.error) errors.push(`Trigger price ${t.error}.`);
    trigger = t.value;
    if (trigger !== null && (trigger === 0n || trigger % market.tick !== 0n)) {
      errors.push(`Trigger price must be a multiple of ${formatTick(market)}.`);
      trigger = null;
    }
    if (trigger !== null && mark !== null) {
      const cond = triggerCondition(form.kind, buy);
      if ((cond === 'markAbove' && mark >= trigger) || (cond === 'markBelow' && mark <= trigger)) warnings.push('The mark price is already past this trigger: the order would trigger at once.');
    }
  }

  let price: bigint | null = null;
  let estimate: BuiltOrder['estimate'] = null;
  if (isMarket) {
    if (form.slippagePpm <= 0n || form.slippagePpm > MAX_SLIPPAGE_PPM) errors.push('The price bound must be between 0.01% and 10%.');
    const reference = triggered ? trigger : mark;
    if (reference === null) {
      if (!triggered) errors.push('No mark price yet: a market order needs one to set its worst price.');
    } else price = worstPrice(reference, buy, form.slippagePpm, market.tick);
    if (price !== null && price <= 0n) {
      errors.push('The worst price would not be positive.');
      price = null;
    }
    if (!triggered && size !== null && size > 0n && price !== null) {
      const inside = levels.filter((l) => (buy ? l.px <= price! : l.px >= price!));
      estimate = estimateMarketFill(inside, size);
      if (!estimate.complete) warnings.push(estimate.filled === 0n ? 'Nothing in the visible book is inside your price bound: the order would not fill.' : 'Only part of this order fills inside your price bound; the rest is cancelled.');
    }
  } else {
    const p = parseAt(form.price, market.priceDecimals, PX_DECIMALS);
    if (p.error) errors.push(`Price ${p.error}.`);
    price = p.value;
    if (price !== null && (price === 0n || price % market.tick !== 0n)) {
      errors.push(`Price must be a multiple of ${formatTick(market)}.`);
      price = null;
    }
    if (price !== null && mark !== null && form.kind === 'limit') {
      const away = mulDiv(price > mark ? price - mark : mark - price, PPM, mark, 'floor');
      if ((buy && price > mark) || (!buy && price < mark)) {
        if (form.tif === 'POST_ONLY') warnings.push('This post-only order is through the mark price and is likely to be rejected.');
        else if (away >= 20_000n) warnings.push(`This limit is ${trim(away, 4, 2)}% through the mark price: it would trade at once against the book.`);
      }
    }
  }

  const tif: TimeInForce = isMarket ? 'IOC' : form.tif;
  if (form.tif === 'POST_ONLY' && isMarket) warnings.push('Post-only does not apply to market orders.');
  if (!account) errors.push('Connect a wallet to place orders.');

  const required = size !== null && price !== null && (!triggered || trigger !== null);
  if (!required && errors.length === 0) errors.push(form.size.trim() === '' ? 'Enter a size.' : triggered && trigger === null ? 'Enter a trigger price.' : 'Enter a price.');
  const message: Message | null =
    errors.length === 0 && account && nonce !== null && size !== null && price !== null
      ? {
          account,
          nonce: nonce.toString(),
          marketId: market.marketId,
          isBuy: buy,
          orderType: ORDER_TYPES.indexOf(form.kind),
          timeInForce: TIFS.indexOf(tif),
          price: price.toString(),
          size: size.toString(),
          reduceOnly: form.reduceOnly,
          triggerPrice: (trigger ?? 0n).toString(),
          clientOrderId,
        }
      : null;
  return { message, errors, warnings, size, price, trigger, isMarket, estimate };
}

function trim(v: bigint, decimals: number, dp: number): string {
  const unit = 10n ** BigInt(decimals - dp);
  const x = v / unit;
  const whole = x / 10n ** BigInt(dp);
  const frac = (x % 10n ** BigInt(dp)).toString().padStart(dp, '0').replace(/0+$/, '');
  return frac ? `${whole}.${frac}` : `${whole}`;
}
export const formatTick = (m: Market) => trim(m.tick, PX_DECIMALS, m.priceDecimals);
export const formatStep = (m: Market) => trim(m.step, SZ_DECIMALS, m.sizeDecimals);

/** A fresh client order id (32 random bytes). */
export function newClientOrderId(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return `0x${Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')}`;
}

/** The exact text of a price at the market's decimals, first rounded to the tick as named. */
export function priceText(px: bigint, market: Market, rounding: 'floor' | 'ceil'): string {
  const ticked = toTick(px, market.tick, rounding);
  return toDecimalString(ticked / 10n ** BigInt(PX_DECIMALS - market.priceDecimals), market.priceDecimals);
}
/** The exact text of a size at the market's decimals (cut to the step, toward zero). */
export function sizeText(size: bigint, market: Market): string {
  const a = size < 0n ? -size : size;
  return toDecimalString((a - (a % market.step)) / 10n ** BigInt(SZ_DECIMALS - market.sizeDecimals), market.sizeDecimals);
}
