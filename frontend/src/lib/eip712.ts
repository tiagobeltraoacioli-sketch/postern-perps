/**
 * The typed data this interface signs, PINNED here. UNAUDITED prototype.
 *
 * Rule: the interface signs only these definitions. `GET /v1/eip712` is read and compared with them; when the node
 * publishes anything different, signing is refused ("this interface is out of date") instead of signing data the
 * interface cannot describe. Nothing the server sends is ever passed to a wallet.
 *
 * PINNED to docs/SPEC.md 0.1.1 section 2 (agent 388, engine branch 318a9a6): domain {PosternPerps, 1, 700770,
 * verifyingContract = the PerpsVault address}, the eight types in their normative field order, fixed scales
 * (price and size 1e8, USDC 1e6), UpdateMargin as uint64 + bool, ApproveAgent.expiresAt in milliseconds.
 * The vault address is taken from the committed build configuration (config/production.json `vault`); while it is
 * null (nothing deployed) there is no domain and nothing can be signed (fail closed).
 * Signed values are the scaled integers of lib/fixed.ts; requests are posted with `"wire": true`, so what is
 * signed is exactly what is sent.
 */
import { PX_DECIMALS, SZ_DECIMALS, USDC_DECIMALS } from './fixed';
import { config } from './config';
import { formatFixed } from './format';
import type { Market, OrderType, TimeInForce } from './types';

export const DOMAIN_NAME = 'PosternPerps';
export const DOMAIN_VERSION = '1';
/** The Bloch L2: the chain wallets are on to deposit (SPEC 2.1 reason a). */
export const DOMAIN_CHAIN_ID = 700770;
export type Domain = { name: string; version: string; chainId: number; verifyingContract: `0x${string}` };
const VAULT_RE = /^0x[0-9a-fA-F]{40}$/;
/** The signing domain of this build, or null when the build names no vault (then nothing can be signed). */
export function domainFor(vault: string | null | undefined): Domain | null {
  if (!vault || !VAULT_RE.test(vault) || /^0x0{40}$/i.test(vault)) return null;
  return { name: DOMAIN_NAME, version: DOMAIN_VERSION, chainId: DOMAIN_CHAIN_ID, verifyingContract: vault.toLowerCase() as `0x${string}` };
}
export const DOMAIN: Domain | null = domainFor(config.vault);
export const EIP712_DOMAIN_TYPE = [
  { name: 'name', type: 'string' },
  { name: 'version', type: 'string' },
  { name: 'chainId', type: 'uint256' },
  { name: 'verifyingContract', type: 'address' },
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
  UpdateMargin: [...head, { name: 'marketId', type: 'uint32' }, { name: 'amount', type: 'uint64' }, { name: 'add', type: 'bool' }],
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
  domain: Domain;
  types: Record<string, readonly Field[]>;
  primaryType: ActionType;
  message: Message;
};

export class NoDomain extends Error {}
export function typedData(type: ActionType, message: Message, domain: Domain | null = DOMAIN): TypedData {
  if (!domain) throw new NoDomain('The vault is not deployed: this build has no signing domain.');
  for (const field of TYPES[type]) if (!(field.name in message)) throw new Error(`${type}: missing field ${field.name}`);
  if (Object.keys(message).length !== TYPES[type].length) throw new Error(`${type}: unexpected field`);
  return { domain, types: { EIP712Domain: EIP712_DOMAIN_TYPE, [type]: TYPES[type] }, primaryType: type, message };
}

/** Compares the node's published definitions with the pinned ones. Returns the first difference, or null. */
export function compareWithNode(published: unknown, pinned: Domain | null = DOMAIN): string | null {
  if (!pinned) return 'this build names no vault, so it has no signing domain';
  if (!published || typeof published !== 'object') return 'the node published no typed-data definitions';
  const { domain, types } = published as { domain?: Record<string, unknown>; types?: Record<string, Field[]> };
  if (!domain || domainProblem(domain, pinned)) return `the signing domain differs (${domain ? domainProblem(domain, pinned) : 'missing'})`;
  if (!types || typeof types !== 'object') return 'the node published no types';
  for (const [name, fields] of Object.entries(TYPES)) {
    const theirs = types[name];
    if (!Array.isArray(theirs)) return `type ${name} is not published`;
    if (theirs.length !== fields.length || fields.some((f, i) => theirs[i]?.name !== f.name || theirs[i]?.type !== f.type)) return `type ${name} differs`;
  }
  return null;
}

/** The domain check done on every signature: exactly these four values. */
export function domainProblem(domain: Record<string, unknown>, pinned: Domain | null = DOMAIN): string | null {
  if (!pinned) return 'no signing domain in this build';
  const keys = Object.keys(domain).sort().join(',');
  if (keys !== 'chainId,name,verifyingContract,version') return 'unexpected domain fields';
  if (domain.name !== pinned.name) return 'wrong domain name';
  if (domain.version !== pinned.version) return 'wrong domain version';
  if (Number(domain.chainId) !== pinned.chainId) return 'wrong signing chain id';
  if (typeof domain.verifyingContract !== 'string' || domain.verifyingContract.toLowerCase() !== pinned.verifyingContract) return 'wrong verifying contract (another deployment)';
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
const utc = (ms: bigint) => new Date(Number(ms)).toISOString().replace('T', ' ').replace(/\.\d+Z$/, ' UTC');

/**
 * Renders a message in words FROM THE VALUES THAT ARE SIGNED (not from the form that produced them), so the
 * review always shows what the wallet is asked for. `markets` resolves the market id; an unknown id is said so.
 */
export function describe(type: ActionType, message: Message, markets: Market[]): Described {
  const market = 'marketId' in message ? markets.find((m) => m.marketId === Number(message.marketId)) : undefined;
  const marketName = 'marketId' in message ? (Number(message.marketId) === 0 && type === 'CancelAll' ? 'Every market' : (market?.symbol ?? `Unknown market #${message.marketId}`)) : '';
  // An unknown market shows the exact integers at the fixed scale (SPEC 1), never a guessed precision (R395-12).
  const pd = market?.priceDecimals ?? PX_DECIMALS;
  const sd = market?.sizeDecimals ?? SZ_DECIMALS;
  const price = (v: string | number | boolean) => `${formatFixed(big(v), PX_DECIMALS, { dp: pd })} USD`;
  const size = (v: string | number | boolean) => `${formatFixed(big(v), SZ_DECIMALS, { dp: sd })} ${market?.base ?? 'contracts'}`;
  const usdc = (v: bigint) => `${formatFixed(v, USDC_DECIMALS, { dp: 2, trim: false })} USDC`;
  const account = { label: 'Account', value: String(message.account) };
  const nonce = { label: 'Nonce', value: String(message.nonce) };
  switch (type) {
    case 'PlaceOrder': {
      const buy = message.isBuy === true;
      // R395-12: an unknown order type or time in force is named as unknown (and refused by lib/sign.ts), never
      // shown as a limit / GTC order.
      const kind = ORDER_TYPES[Number(message.orderType)];
      if (!kind) return unknownDescription(`order type #${message.orderType}`, rowsOf(message));
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
        { label: 'Time in force', value: TIF_WORDS[TIFS[Number(message.timeInForce)]] ?? `UNKNOWN (#${message.timeInForce})` },
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
      const add = message.add === true;
      return {
        title: `${add ? 'Add' : 'Remove'} ${usdc(amount)} isolated margin ${add ? 'to' : 'from'} ${marketName}`,
        tone: 'neutral',
        rows: [{ label: 'Market', value: marketName }, { label: add ? 'Add to position' : 'Remove from position', value: usdc(amount), emphasis: true }, account, nonce],
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

function rowsOf(message: Message) {
  return Object.entries(message).map(([label, v]) => ({ label, value: String(v) }));
}
function unknownDescription(what: string, rows: Described['rows']): Described {
  return { title: `Unknown request: ${what}`, tone: 'danger', rows, notes: ['This interface cannot describe this request, so it will not sign it.'] };
}

function triggerDirection(kind: OrderType, buy: boolean): string {
  // A stop triggers against the position it protects; a take-profit with it.
  const stop = kind === 'stopMarket' || kind === 'stopLimit';
  return (stop ? buy : !buy) ? 'rises to' : 'falls to';
}
export function triggerCondition(kind: OrderType, buy: boolean): 'markAbove' | 'markBelow' {
  return triggerDirection(kind, buy) === 'rises to' ? 'markAbove' : 'markBelow';
}
