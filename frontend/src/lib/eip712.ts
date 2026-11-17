/**
 * The typed data this interface signs, PINNED here. UNAUDITED prototype.
 *
 * Rule: the interface signs only these definitions. `GET /v1/eip712` is read and compared with them; when the node
 * publishes anything different, signing is refused ("this interface is out of date") instead of signing data the
 * interface cannot describe. Nothing the server sends is ever passed to a wallet.
 *
 * PROVISIONAL until docs/SPEC.md (agent 350) fixes names, order and scales (cross-area log, 2026-11-17 03:05 UTC).
 * Signed values are the scaled integers of lib/fixed.ts; requests are posted with `"wire": true`, so what is
 * signed is exactly what is sent.
 */
import { PX_DECIMALS, SZ_DECIMALS, USDC_DECIMALS } from './fixed';
import { formatFixed } from './format';
import type { Market, OrderType, TimeInForce } from './types';

export const DOMAIN = { name: 'PosternPerps', version: '1', chainId: 7007701 } as const;
export const EIP712_DOMAIN_TYPE = [
  { name: 'name', type: 'string' },
  { name: 'version', type: 'string' },
  { name: 'chainId', type: 'uint256' },
] as const;

type Field = { name: string; type: string };
const head: Field[] = [
  { name: 'account', type: 'address' },
  { name: 'nonce', type: 'uint64' },
];
export const TYPES = {
  PlaceOrder: [
    ...head,
    { name: 'marketId', type: 'uint32' },
    { name: 'isBuy', type: 'bool' },
    { name: 'orderType', type: 'uint8' },
    { name: 'timeInForce', type: 'uint8' },
    { name: 'price', type: 'uint64' },
    { name: 'size', type: 'uint64' },
    { name: 'reduceOnly', type: 'bool' },
    { name: 'triggerPrice', type: 'uint64' },
    { name: 'clientOrderId', type: 'bytes32' },
  ],
  CancelOrder: [...head, { name: 'marketId', type: 'uint32' }, { name: 'orderId', type: 'uint64' }],
  CancelAll: [...head, { name: 'marketId', type: 'uint32' }],
  UpdateLeverage: [...head, { name: 'marketId', type: 'uint32' }, { name: 'leverage', type: 'uint32' }, { name: 'isolated', type: 'bool' }],
  UpdateMargin: [...head, { name: 'marketId', type: 'uint32' }, { name: 'amount', type: 'int64' }],
  Withdraw: [...head, { name: 'amount', type: 'uint64' }, { name: 'recipient', type: 'address' }],
  ApproveAgent: [...head, { name: 'agent', type: 'address' }, { name: 'name', type: 'string' }, { name: 'expiresAt', type: 'uint64' }],
  RevokeAgent: [...head, { name: 'agent', type: 'address' }],
} satisfies Record<string, Field[]>;
export type ActionType = keyof typeof TYPES;

/** What an agent wallet may sign. Withdraw, ApproveAgent and RevokeAgent need the main key, always. */
export const AGENT_ALLOWED: readonly ActionType[] = ['PlaceOrder', 'CancelOrder', 'CancelAll', 'UpdateLeverage', 'UpdateMargin'];
/** Actions that only reduce exposure or leave: open to a restricted region under the exit-only policy. */
export const EXIT_ACTIONS: readonly ActionType[] = ['CancelOrder', 'CancelAll', 'Withdraw'];

export const ORDER_TYPES: OrderType[] = ['limit', 'market', 'stopMarket', 'stopLimit', 'takeProfitMarket', 'takeProfitLimit'];
export const TIFS: TimeInForce[] = ['GTC', 'IOC', 'POST_ONLY'];

/** Message values as the wallet receives them: integers as decimal strings, booleans, hex. */
export type Message = Record<string, string | number | boolean>;
export type TypedData = {
  domain: typeof DOMAIN;
  types: Record<string, readonly Field[]>;
  primaryType: ActionType;
  message: Message;
};

export function typedData(type: ActionType, message: Message): TypedData {
  for (const field of TYPES[type]) if (!(field.name in message)) throw new Error(`${type}: missing field ${field.name}`);
  if (Object.keys(message).length !== TYPES[type].length) throw new Error(`${type}: unexpected field`);
  return { domain: DOMAIN, types: { EIP712Domain: EIP712_DOMAIN_TYPE, [type]: TYPES[type] }, primaryType: type, message };
}

/** Compares the node's published definitions with the pinned ones. Returns the first difference, or null. */
export function compareWithNode(published: unknown): string | null {
  if (!published || typeof published !== 'object') return 'the node published no typed-data definitions';
  const { domain, types } = published as { domain?: Record<string, unknown>; types?: Record<string, Field[]> };
  if (!domain || domain.name !== DOMAIN.name || domain.version !== DOMAIN.version || Number(domain.chainId) !== DOMAIN.chainId)
    return 'the signing domain differs';
  if ('verifyingContract' in domain || 'salt' in domain) return 'the signing domain has fields this interface does not know';
  if (!types || typeof types !== 'object') return 'the node published no types';
  for (const [name, fields] of Object.entries(TYPES)) {
    const theirs = types[name];
    if (!Array.isArray(theirs)) return `type ${name} is not published`;
    if (theirs.length !== fields.length || fields.some((f, i) => theirs[i]?.name !== f.name || theirs[i]?.type !== f.type)) return `type ${name} differs`;
  }
  return null;
}

/** The domain check done on every signature: these three values and nothing else. */
export function domainProblem(domain: { name?: unknown; version?: unknown; chainId?: unknown; [k: string]: unknown }): string | null {
  const keys = Object.keys(domain).sort().join(',');
  if (keys !== 'chainId,name,version') return 'unexpected domain fields';
  if (domain.name !== DOMAIN.name) return 'wrong domain name';
  if (domain.version !== DOMAIN.version) return 'wrong domain version';
  if (domain.chainId !== DOMAIN.chainId) return 'wrong signing chain id';
  return null;
}

/* ---------- In words ---------- */

export type Tone = 'neutral' | 'long' | 'short' | 'danger';
export type Described = {
  /** One sentence: what signing this does. */
  title: string;
  tone: Tone;
  /** Label and value pairs in the order a person checks them. */
  rows: { label: string; value: string; emphasis?: boolean }[];
  /** What this signature can never do, or what to watch. */
  notes: string[];
};

const ORDER_WORDS: Record<OrderType, string> = {
  limit: 'Limit',
  market: 'Market',
  stopMarket: 'Stop market',
  stopLimit: 'Stop limit',
  takeProfitMarket: 'Take profit market',
  takeProfitLimit: 'Take profit limit',
};
const TIF_WORDS: Record<TimeInForce, string> = {
  GTC: 'Good till cancelled',
  IOC: 'Immediate or cancel (what does not fill at once is cancelled)',
  POST_ONLY: 'Post only (rejected if it would trade at once)',
};
export const orderTypeWords = (t: OrderType) => ORDER_WORDS[t];

const big = (v: string | number | boolean) => BigInt(v as string);
const utc = (seconds: bigint) => new Date(Number(seconds) * 1000).toISOString().replace('T', ' ').replace(/\.\d+Z$/, ' UTC');

/**
 * Renders a message in words FROM THE VALUES THAT ARE SIGNED (not from the form that produced them), so the
 * review always shows what the wallet is asked for. `markets` resolves the market id; an unknown id is said so.
 */
export function describe(type: ActionType, message: Message, markets: Market[]): Described {
  const market = 'marketId' in message ? markets.find((m) => m.marketId === Number(message.marketId)) : undefined;
  const marketName = 'marketId' in message ? (Number(message.marketId) === 0 && type === 'CancelAll' ? 'Every market' : (market?.symbol ?? `Unknown market #${message.marketId}`)) : '';
  const pd = market?.priceDecimals ?? 2;
  const sd = market?.sizeDecimals ?? 4;
  const price = (v: string | number | boolean) => `${formatFixed(big(v), PX_DECIMALS, { dp: pd })} USD`;
  const size = (v: string | number | boolean) => `${formatFixed(big(v), SZ_DECIMALS, { dp: sd })} ${market?.base ?? 'contracts'}`;
  const usdc = (v: bigint) => `${formatFixed(v, USDC_DECIMALS, { dp: 2, trim: false })} USDC`;
  const account = { label: 'Account', value: String(message.account) };
  const nonce = { label: 'Nonce', value: String(message.nonce) };
  switch (type) {
    case 'PlaceOrder': {
      const buy = message.isBuy === true;
      const kind = ORDER_TYPES[Number(message.orderType)] ?? 'limit';
      const isMarket = kind === 'market' || kind === 'stopMarket' || kind === 'takeProfitMarket';
      const hasTrigger = kind !== 'limit' && kind !== 'market';
      const rows = [
        { label: 'Market', value: marketName, emphasis: true },
        { label: 'Side', value: buy ? 'Buy / Long' : 'Sell / Short', emphasis: true },
        { label: 'Order type', value: ORDER_WORDS[kind] },
        { label: 'Size', value: size(message.size), emphasis: true },
        isMarket
          ? { label: buy ? 'Worst price accepted' : 'Lowest price accepted', value: price(message.price) }
          : { label: 'Limit price', value: price(message.price), emphasis: true },
        ...(hasTrigger ? [{ label: 'Triggers when mark price', value: `${triggerDirection(kind, buy)} ${price(message.triggerPrice)}` }] : []),
        { label: 'Time in force', value: TIF_WORDS[TIFS[Number(message.timeInForce)] ?? 'GTC'] },
        { label: 'Reduce only', value: message.reduceOnly ? 'Yes: can only shrink a position' : 'No: can open or grow a position' },
        account,
        nonce,
      ];
      return {
        title: `${buy ? 'Buy' : 'Sell'} ${size(message.size)} of ${marketName}${isMarket ? ' at market' : ` at ${price(message.price)}`}`,
        tone: buy ? 'long' : 'short',
        rows,
        notes: ['An order cannot move funds out of your account. Leveraged positions can be liquidated.'],
      };
    }
    case 'CancelOrder':
      return {
        title: `Cancel order ${message.orderId} on ${marketName}`,
        tone: 'neutral',
        rows: [{ label: 'Market', value: marketName }, { label: 'Order', value: `#${message.orderId}`, emphasis: true }, account, nonce],
        notes: ['Cancelling places no order and moves no funds.'],
      };
    case 'CancelAll':
      return {
        title: `Cancel all open orders: ${marketName.toLowerCase()}`,
        tone: 'neutral',
        rows: [{ label: 'Scope', value: marketName, emphasis: true }, account, nonce],
        notes: ['Cancelling places no order and moves no funds.'],
      };
    case 'UpdateLeverage':
      return {
        title: `Set ${marketName} to ${message.leverage}x ${message.isolated ? 'isolated' : 'cross'} margin`,
        tone: 'neutral',
        rows: [
          { label: 'Market', value: marketName },
          { label: 'Leverage', value: `${message.leverage}x`, emphasis: true },
          { label: 'Margin mode', value: message.isolated ? 'Isolated: only the margin of this position is at risk' : 'Cross: the whole account balance backs this position', emphasis: true },
          account,
          nonce,
        ],
        notes: ['Higher leverage moves the liquidation price closer to the market price.'],
      };
    case 'UpdateMargin': {
      const amount = big(message.amount);
      return {
        title: `${amount < 0n ? 'Remove' : 'Add'} ${usdc(amount < 0n ? -amount : amount)} isolated margin on ${marketName}`,
        tone: 'neutral',
        rows: [{ label: 'Market', value: marketName }, { label: amount < 0n ? 'Remove from position' : 'Add to position', value: usdc(amount < 0n ? -amount : amount), emphasis: true }, account, nonce],
        notes: ['Margin moves between your account balance and this position. Nothing leaves your account.'],
      };
    }
    case 'Withdraw':
      return {
        title: `Withdraw ${usdc(big(message.amount))} to ${message.recipient}`,
        tone: 'danger',
        rows: [
          { label: 'Amount', value: usdc(big(message.amount)), emphasis: true },
          { label: 'Recipient on the Bloch L2', value: String(message.recipient), emphasis: true },
          { label: 'From account', value: String(message.account) },
          nonce,
        ],
        notes: [
          'This moves funds OUT of your trading account. Check the recipient address character by character.',
          'Only your main wallet can sign a withdrawal. An agent key never can.',
        ],
      };
    case 'ApproveAgent':
      return {
        title: `Allow agent key ${message.agent} to trade for this account`,
        tone: 'danger',
        rows: [
          { label: 'Agent key', value: String(message.agent), emphasis: true },
          { label: 'Name', value: String(message.name) },
          { label: 'Valid until', value: utc(big(message.expiresAt)), emphasis: true },
          account,
          nonce,
        ],
        notes: [
          'The agent key CAN place and cancel orders and change leverage and isolated margin for this account, without asking your wallet each time.',
          'The agent key can NEVER withdraw, never change the withdrawal address and never approve another key.',
          'Whoever holds the agent key can trade this account until it expires or you revoke it.',
        ],
      };
    case 'RevokeAgent':
      return {
        title: `Revoke agent key ${message.agent}`,
        tone: 'neutral',
        rows: [{ label: 'Agent key', value: String(message.agent), emphasis: true }, account, nonce],
        notes: ['After this is included, that key can no longer trade for this account. Its open orders stay until cancelled.'],
      };
  }
}

function triggerDirection(kind: OrderType, buy: boolean): string {
  // A stop triggers against the position it protects; a take-profit with it.
  const stop = kind === 'stopMarket' || kind === 'stopLimit';
  return (stop ? buy : !buy) ? 'rises to' : 'falls to';
}
export function triggerCondition(kind: OrderType, buy: boolean): 'markAbove' | 'markBelow' {
  return triggerDirection(kind, buy) === 'rises to' ? 'markAbove' : 'markBelow';
}
