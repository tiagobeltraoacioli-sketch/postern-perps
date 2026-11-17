/**
 * The one path from a filled form to a submitted action. UNAUDITED prototype.
 *
 * Every action goes through `prepare` (build typed data from the PINNED types, describe it in words from the signed
 * values, list every refusal) and then `signAndSubmit`, which re-runs every refusal immediately before the wallet is
 * asked (state may have changed while the review was open), signs, checks the signature locally (65 bytes, low-s,
 * v 27/28, recovers to the signer the user chose) and only then posts it. A refusal is never a warning: the
 * signature is not requested.
 *
 * Refusals (each has a test in src/test/sign.test.ts):
 *  - no signer, or the signer is not the connected main account or one of its unexpired agent keys on this device;
 *  - `message.account` is not the connected main account;
 *  - the node's typed-data definitions were not read yet, or differ from the pinned ones (lib/eip712.ts);
 *  - the market named by `marketId` is not in the pinned table, or the exchange publishes it with other signing
 *    fields (lib/pins.ts), or two markets claim the id;
 *  - an agent key for an action only the main key may sign (Withdraw, ApproveAgent, RevokeAgent);
 *  - the address is sanctioned (everything refused), or the region is restricted or not known (only exit actions:
 *    CancelOrder, CancelAll, Withdraw, reduce-only PlaceOrder; docs/API.md section 6);
 *  - the market is halted (only cancels) or close-only (only cancels and reduce-only orders);
 *  - the sequencer reports it is not running;
 *  - a value the engine would read differently from the review: a PlaceOrder without a positive price (a market
 *    order always carries its worst price), a price off the tick, a size off the step or outside the market's
 *    bounds, a uint64 out of range, a withdrawal to the zero address, an agent approval for the account itself, in
 *    the past, or longer than the stated maximum.
 */
import { recoverTypedDataAddress, type Hex } from 'viem';
import { AGENT_ALLOWED, DOMAIN, TYPES, describe, domainProblem, typedData, type ActionType, type Described, type Message, type TypedData } from './eip712';
import { signingProblem } from './pins';
import type { Market, WireGeo, WireStatus } from './types';
import { validAddress } from '../wallet/wallet';

export const MAX_UINT64 = (1n << 64n) - 1n;
export const MAX_INT64 = (1n << 63n) - 1n;
/** Longest agent approval this interface signs (seconds). Presentation rule of the interface; the engine may differ. */
export const MAX_AGENT_SECONDS = 180n * 86_400n;
const SECP256K1_N = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n;

export type Signer = {
  kind: 'wallet' | 'agent';
  /** Lower-case address that produces the signature. */
  address: string;
  /** Agent keys only: unix seconds after which the exchange no longer accepts it. */
  expiresAt?: number;
  sign(data: TypedData): Promise<string>;
};

/** Everything the refusals read. `eip712Problem` undefined means "not read yet". */
export type Guard = {
  account: string | null;
  eip712Problem: string | null | undefined;
  markets: Market[];
  tableProblems: ReadonlyMap<string, string>;
  geo: WireGeo | null;
  status: WireStatus | null;
  /** Unix ms, for agent expiry and approval bounds. */
  now: number;
};

export type Prepared = {
  type: ActionType;
  message: Message;
  data: TypedData;
  described: Described;
  refusals: string[];
};

/** Actions that let a user leave: open under the exit-only policy (docs/API.md section 6). */
export function isExitAction(type: ActionType, message: Message): boolean {
  return type === 'CancelOrder' || type === 'CancelAll' || type === 'Withdraw' || (type === 'PlaceOrder' && message.reduceOnly === true);
}

const uint = (v: unknown) => {
  try {
    const b = BigInt(v as string);
    return b >= 0n && b <= MAX_UINT64 ? b : null;
  } catch {
    return null;
  }
};

/** Value refusals that depend only on the message and the pinned market (no network state). */
export function valueProblems(type: ActionType, message: Message, markets: Market[], now: number): string[] {
  const out: string[] = [];
  for (const field of TYPES[type]) {
    const v = message[field.name];
    if (field.type === 'uint64' && uint(v) === null) out.push(`${field.name} is not a 64-bit unsigned integer`);
    if (field.type === 'uint32' && (uint(v) === null || uint(v)! > 0xffffffffn)) out.push(`${field.name} is not a 32-bit unsigned integer`);
    if (field.type === 'uint8' && (uint(v) === null || uint(v)! > 255n)) out.push(`${field.name} is not an 8-bit unsigned integer`);
    if (field.type === 'int64') {
      let b: bigint | null = null;
      try {
        b = BigInt(v as string);
      } catch {
        /* null */
      }
      if (b === null || b > MAX_INT64 || b < -MAX_INT64 - 1n) out.push(`${field.name} is not a 64-bit integer`);
    }
    if (field.type === 'address' && !validAddress(v)) out.push(`${field.name} is not an address`);
    if (field.type === 'bool' && typeof v !== 'boolean') out.push(`${field.name} is not true or false`);
    if (field.type === 'bytes32' && !(typeof v === 'string' && /^0x[0-9a-f]{64}$/i.test(v))) out.push(`${field.name} is not 32 bytes`);
  }
  if (out.length) return out;
  const market = 'marketId' in message ? markets.find((m) => m.marketId === Number(message.marketId)) : undefined;
  switch (type) {
    case 'PlaceOrder': {
      if (!market) break;
      const price = BigInt(message.price as string);
      const size = BigInt(message.size as string);
      const trigger = BigInt(message.triggerPrice as string);
      const kind = Number(message.orderType);
      if (kind > 5) out.push('unknown order type');
      if (Number(message.timeInForce) > 2) out.push('unknown time in force');
      if (price <= 0n) out.push('an order must carry a positive price (a market order carries its worst price)');
      else if (price % market.tick !== 0n) out.push('the price is not a multiple of the tick');
      if (size <= 0n) out.push('the size must be positive');
      else if (size % market.step !== 0n) out.push('the size is not a multiple of the step');
      else if (size < market.minSize) out.push('the size is below the market minimum');
      else if (size > market.maxSize) out.push('the size is above the market maximum');
      const triggered = kind >= 2;
      if (triggered && (trigger <= 0n || trigger % market.tick !== 0n)) out.push('the trigger price must be positive and on the tick');
      if (!triggered && trigger !== 0n) out.push('only stop and take-profit orders carry a trigger price');
      if ((kind === 1 || kind === 2 || kind === 4) && Number(message.timeInForce) !== 1) out.push('a market order must be immediate-or-cancel');
      if (Number(message.timeInForce) === 2 && kind !== 0 && kind !== 3 && kind !== 5) out.push('post-only applies to limit orders only');
      break;
    }
    case 'UpdateLeverage': {
      if (!market) break;
      const lev = Number(message.leverage);
      if (!Number.isInteger(lev) || lev < 1 || lev > market.maxLeverage) out.push(`leverage must be 1 to ${market.maxLeverage}`);
      break;
    }
    case 'UpdateMargin':
      if (BigInt(message.amount as string) === 0n) out.push('the amount must not be zero');
      break;
    case 'Withdraw':
      if (BigInt(message.amount as string) <= 0n) out.push('the amount must be positive');
      break;
    case 'ApproveAgent': {
      const expires = BigInt(message.expiresAt as string);
      const nowS = BigInt(Math.floor(now / 1000));
      if (String(message.agent).toLowerCase() === String(message.account).toLowerCase()) out.push('the agent key must not be the account itself');
      if (expires <= nowS) out.push('the approval must end in the future');
      if (expires > nowS + MAX_AGENT_SECONDS) out.push('the approval may last at most 180 days');
      const name = String(message.name);
      if (!/^[\x20-\x7e]{1,32}$/.test(name)) out.push('the agent name must be 1 to 32 plain characters');
      break;
    }
    default:
      break;
  }
  return out;
}

/** Every refusal of `type`/`message` signed by `signer` under `guard`. Empty: the wallet may be asked. */
export function refusals(type: ActionType, message: Message, signer: Signer | null, guard: Guard): string[] {
  const out: string[] = [];
  const account = guard.account?.toLowerCase() ?? null;
  if (!account) out.push('Connect a wallet first.');
  if (!signer) out.push('No key is available to sign.');
  if (account && String(message.account).toLowerCase() !== account) out.push('The request names another account than the connected wallet.');
  if (signer && account) {
    if (signer.kind === 'wallet' && signer.address !== account) out.push('The wallet account changed. Review again.');
    if (signer.kind === 'agent') {
      if (!AGENT_ALLOWED.includes(type)) out.push('This action needs your main wallet. An agent key can never sign it.');
      if (signer.expiresAt !== undefined && signer.expiresAt * 1000 <= guard.now) out.push('The agent key on this device has expired. Approve a new one or sign with your wallet.');
    }
  }
  if (guard.eip712Problem === undefined) out.push('The exchange signing definitions have not been checked yet.');
  else if (guard.eip712Problem) out.push(`This interface is out of date with the exchange (${guard.eip712Problem}). Signing is refused.`);
  if ('marketId' in message && !(type === 'CancelAll' && Number(message.marketId) === 0)) {
    const market = guard.markets.find((m) => m.marketId === Number(message.marketId));
    if (!market) out.push(`Market #${message.marketId} is not published by the exchange.`);
    else {
      const problem = guard.tableProblems.get(market.symbol) ?? signingProblem(market);
      if (problem) out.push(`Signing refused: ${problem}.`);
      const exitOnly = type === 'CancelOrder' || type === 'CancelAll';
      if (market.status === 'halted' && !exitOnly) out.push(`${market.symbol} is halted: only cancelling is possible.`);
      if (market.status === 'settled' && !exitOnly) out.push(`${market.symbol} is settled.`);
      if (market.status === 'closeOnly' && !exitOnly && !(type === 'PlaceOrder' && message.reduceOnly === true))
        out.push(`${market.symbol} is close-only: only reduce-only orders and cancels are accepted.`);
    }
  }
  const geo = guard.geo;
  if (geo?.sanctioned) out.push('This address cannot use PosternDEX Perpetuals.');
  else if (!isExitAction(type, message)) {
    if (!geo) out.push('Your region could not be confirmed. Only cancelling, reducing and withdrawing are possible.');
    else if (geo.restricted || geo.policy !== 'open') out.push('PosternDEX Perpetuals is not available in your region. Only cancelling, reducing and withdrawing are possible.');
  }
  if (guard.status && guard.status.sequencer.state !== 'running') out.push(`The exchange is not accepting actions (sequencer ${guard.status.sequencer.state}).`);
  for (const p of valueProblems(type, message, guard.markets, guard.now)) out.push(`Refused: ${p}.`);
  return [...new Set(out)];
}

export function prepare(type: ActionType, message: Message, signer: Signer | null, guard: Guard): Prepared {
  const data = typedData(type, message);
  return { type, message, data, described: describe(type, message, guard.markets), refusals: refusals(type, message, signer, guard) };
}

/** The typed data in the form viem verifies (integers as bigint). */
export function forViem(data: TypedData) {
  const message: Record<string, unknown> = {};
  for (const field of TYPES[data.primaryType]) {
    const v = data.message[field.name];
    message[field.name] = /^u?int/.test(field.type) ? BigInt(v as string) : v;
  }
  return { domain: { ...data.domain }, types: { [data.primaryType]: TYPES[data.primaryType] }, primaryType: data.primaryType, message } as const;
}

/** The JSON a wallet receives for eth_signTypedData_v4 (integers as decimal strings). */
export function forWallet(data: TypedData): string {
  return JSON.stringify(data);
}

/** Checks a signature's form: 65 bytes, low-s, r and s in range, v 27 or 28. */
export function signatureProblem(signature: unknown): string | null {
  if (typeof signature !== 'string' || !/^0x[0-9a-fA-F]{130}$/.test(signature)) return 'the signature is not 65 bytes';
  const r = BigInt(`0x${signature.slice(2, 66)}`);
  const s = BigInt(`0x${signature.slice(66, 130)}`);
  const v = parseInt(signature.slice(130, 132), 16);
  if (r < 1n || r >= SECP256K1_N || s < 1n || s >= SECP256K1_N) return 'the signature values are out of range';
  if (s > SECP256K1_N / 2n) return 'the signature is not in low-s form';
  if (v !== 27 && v !== 28) return 'the signature recovery byte is not 27 or 28';
  return null;
}

export class Refused extends Error {
  constructor(readonly reasons: string[]) {
    super(reasons[0] ?? 'Refused.');
  }
}

/**
 * Re-checks, signs, verifies, submits. `guardNow` reads the CURRENT guard (not the one the review was built with).
 * `submit` posts the action (api.action in the app, a stub in tests).
 */
export async function signAndSubmit<T>(
  prepared: Prepared,
  signer: Signer,
  guardNow: () => Guard,
  submit: (body: { type: string; message: Message; signature: string }) => Promise<T>,
): Promise<T> {
  const again = refusals(prepared.type, prepared.message, signer, guardNow());
  if (again.length) throw new Refused(again);
  const domain = domainProblem(prepared.data.domain as never);
  if (domain) throw new Refused([`Refused: ${domain}.`]);
  if (prepared.data.domain.chainId !== DOMAIN.chainId) throw new Refused(['Refused: wrong signing chain id.']);
  const signature = await signer.sign(prepared.data);
  const form = signatureProblem(signature);
  if (form) throw new Refused([`The wallet returned an unusable signature: ${form}. Nothing was submitted.`]);
  let recovered: string;
  try {
    recovered = (await recoverTypedDataAddress({ ...forViem(prepared.data), signature: signature as Hex } as never)).toLowerCase();
  } catch {
    throw new Refused(['The signature could not be checked. Nothing was submitted.']);
  }
  if (recovered !== signer.address) throw new Refused(['The wallet signed with another account than the one shown. Nothing was submitted.']);
  return submit({ type: prepared.type, message: prepared.message, signature });
}

/** The main wallet as a signer (eth_signTypedData_v4). */
export function walletSigner(address: string, request: (args: { method: string; params?: unknown[] }) => Promise<unknown>): Signer {
  const lower = address.toLowerCase();
  return {
    kind: 'wallet',
    address: lower,
    async sign(data) {
      const result = await request({ method: 'eth_signTypedData_v4', params: [address, forWallet(data)] });
      return String(result);
    },
  };
}
