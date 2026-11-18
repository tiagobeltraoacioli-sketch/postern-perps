/**
 * The vault as the page sees it, and every transaction the page can ask a wallet for. UNAUDITED prototype.
 *
 * Read docs/VAULT.md first. What this file guarantees, each with a test (src/test/vault.test.ts) and exercised
 * against the real contract on a local anvil (integration/vault/anvil.mjs):
 *
 *  - The only contracts ever called are the vault and the collateral token NAMED IN THE BUILD CONFIGURATION; an
 *    address from the API is never a transaction target. The vault's own `token()` must equal the configured token
 *    or nothing is built.
 *  - Every transaction is one of a closed list (`TxKind`), built by `buildTx` from typed values; the review shown to
 *    the user is produced by DECODING the calldata that will be sent (`describeTx`), not from the form.
 *  - Approvals are for the exact deposit amount, never unlimited.
 *  - A claim (withdrawal record or escape leaf) is hashed and walked to a root in the page and compared with the root
 *    the vault holds on the L2 before the wallet is asked (`checkWithdrawalClaim`, `checkEscapeClaim`): a record the
 *    API altered is refused here.
 *  - Before the wallet is asked, the call is simulated from the user's address on the L2 endpoint; a revert is shown
 *    in words and the wallet is not asked (`simulate`).
 *  - Amounts are integers of bUSDC base units (6 decimals) end to end; the escape estimate uses the vault's own
 *    formula and rounding (floor).
 */
import { decodeErrorResult, decodeEventLog, decodeFunctionData, decodeFunctionResult, encodeAbiParameters, encodeFunctionData, keccak256, stringToHex, toEventSelector, type Hex } from 'viem';
import { REVERT_WORDS, TOKEN_ABI, VAULT_ABI } from './abi';
import { L2, L2Error, type Receipt } from './l2';
import { accountLeaf, rootFromProof, withdrawalLeaf, type AccountLeaf, type WithdrawalRecord } from './merkle';
import { formatFixed } from '../lib/format';
import { USDC_DECIMALS } from '../lib/fixed';

export type Deployment = { chainId: number; vault: Hex; token: Hex };
const ADDRESS = /^0x[0-9a-fA-F]{40}$/;
const MAX_U64 = (1n << 64n) - 1n;
export const SURPLUS_DELAY_S = 90n * 86_400n;
export const FORCED_KINDS = ['cancelAll', 'closeAll', 'withdraw'] as const;
export type ForcedKind = (typeof FORCED_KINDS)[number];

/** The deployment of this build, or null when the configuration names no vault or token (nothing can be built). */
export function deploymentOf(c: { l2ChainId: number; vault: string | null; collateral: string | null }): Deployment | null {
  if (!c.vault || !c.collateral || !ADDRESS.test(c.vault) || !ADDRESS.test(c.collateral)) return null;
  if (/^0x0{40}$/.test(c.vault) || /^0x0{40}$/.test(c.collateral) || c.vault.toLowerCase() === c.collateral.toLowerCase()) return null;
  return { chainId: c.l2ChainId, vault: c.vault.toLowerCase() as Hex, token: c.collateral.toLowerCase() as Hex };
}

export const usdc = (v: bigint) => `${formatFixed(v, USDC_DECIMALS, { dp: 6, trim: true, minDp: 2 })} bUSDC`;

/* ---------------------------------------------------------------- view */

export type Payout = { recipient: Hex; total: bigint; paid: bigint; readyAt: bigint };
export type VaultView = {
  /** L2 block the whole view was read at, and its timestamp (the vault's clock, seconds). */
  block: bigint;
  now: bigint;
  paused: boolean;
  pausedUntil: bigint;
  escapeAt: bigint;
  /** 0 none, 1 silence, 2 an overdue forced request (VAULT.md `escapeReason`). */
  escapeReason: number;
  systemSharesProven: boolean;
  escapePayable: bigint;
  subordinatedTotal: bigint;
  minDeposit: bigint;
  cap: bigint;
  custody: bigint;
  largeThreshold: bigint;
  largeDelay: bigint;
  forcedDelay: bigint;
  escapeDelay: bigint;
  availableToRelease: bigint;
  checkpointCount: bigint;
  lastCheckpointAt: bigint;
  depositCount: bigint;
  forcedCount: bigint;
  latest: { height: bigint; time: bigint; nextDepositIndex: bigint; nextForcedIndex: bigint; withdrawalCount: bigint; withdrawalsTotal: bigint; totalEscape: bigint; accountsRoot: Hex; stateRoot: Hex };
  /** The connected account, when one was given. */
  account: null | {
    address: Hex;
    balance: bigint;
    allowance: bigint;
    hasDeposited: boolean;
    /** Index of the account's last forced request, or null. */
    forcedIndex: bigint | null;
    /** Its deadline (L2 seconds), when there is one. */
    forcedDeadline: bigint | null;
    /** True while that request is not yet acknowledged by an accepted checkpoint. */
    forcedOpen: boolean;
    escapePayout: Payout;
  };
};

const vaultCall = (functionName: string, args: unknown[] = []) => encodeFunctionData({ abi: VAULT_ABI, functionName, args } as never);
const vaultResult = (functionName: string, data: Hex): unknown => decodeFunctionResult({ abi: VAULT_ABI, functionName, data } as never);
const big = (v: unknown): bigint => (typeof v === 'bigint' ? v : BigInt(v as number));

export class VaultMismatch extends Error {}

/** Reads the whole view at one block. Refuses when the vault's token is not the configured collateral. */
export async function readVault(l2: L2, d: Deployment, account: string | null): Promise<VaultView> {
  const head = await l2.head('latest');
  const who = account && ADDRESS.test(account) ? (account.toLowerCase() as Hex) : null;
  const names = ['token', 'paused', 'pausedUntil', 'escapeAt', 'escapeReason', 'systemSharesProven', 'escapePayable', 'subordinatedTotal', 'minDeposit', 'cap', 'custody', 'largeThreshold', 'largeDelay', 'forcedDelay', 'escapeDelay', 'availableToRelease', 'checkpointCount', 'lastCheckpointAt', 'depositCount', 'forcedCount', 'latest'] as const;
  const calls: { to: Hex; data: Hex }[] = names.map((n) => ({ to: d.vault, data: vaultCall(n) }));
  if (who) {
    calls.push({ to: d.token, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'balanceOf', args: [who] }) });
    calls.push({ to: d.token, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'allowance', args: [who, d.vault] }) });
    calls.push({ to: d.vault, data: vaultCall('hasDeposited', [who]) });
    calls.push({ to: d.vault, data: vaultCall('lastForcedPlusOne', [who]) });
    calls.push({ to: d.vault, data: vaultCall('payouts', [escapeKey(who)]) });
  }
  const raw = await l2.calls(calls, head.number);
  const v = Object.fromEntries(names.map((n, i) => [n, vaultResult(n, raw[i])])) as Record<(typeof names)[number], unknown>;
  if (String(v.token).toLowerCase() !== d.token) throw new VaultMismatch('The vault on the L2 holds another token than this interface is built for. Nothing is built or sent.');
  const latest = v.latest as readonly [bigint, bigint, bigint, bigint, bigint, bigint, bigint, Hex, Hex];
  const view: VaultView = {
    block: head.number,
    now: head.timestamp,
    paused: v.paused as boolean,
    pausedUntil: big(v.pausedUntil),
    escapeAt: big(v.escapeAt),
    escapeReason: Number(v.escapeReason),
    systemSharesProven: v.systemSharesProven as boolean,
    escapePayable: big(v.escapePayable),
    subordinatedTotal: big(v.subordinatedTotal),
    minDeposit: big(v.minDeposit),
    cap: big(v.cap),
    custody: big(v.custody),
    largeThreshold: big(v.largeThreshold),
    largeDelay: big(v.largeDelay),
    forcedDelay: big(v.forcedDelay),
    escapeDelay: big(v.escapeDelay),
    availableToRelease: big(v.availableToRelease),
    checkpointCount: big(v.checkpointCount),
    lastCheckpointAt: big(v.lastCheckpointAt),
    depositCount: big(v.depositCount),
    forcedCount: big(v.forcedCount),
    latest: { height: latest[0], time: latest[1], nextDepositIndex: latest[2], nextForcedIndex: latest[3], withdrawalCount: latest[4], withdrawalsTotal: latest[5], totalEscape: latest[6], accountsRoot: latest[7], stateRoot: latest[8] },
    account: null,
  };
  if (who) {
    const base = names.length;
    const lastPlusOne = big(vaultResult('lastForcedPlusOne', raw[base + 3]));
    const forcedIndex = lastPlusOne === 0n ? null : lastPlusOne - 1n;
    const forcedDeadline = forcedIndex === null ? null : big(vaultResult('forcedDeadline', await l2.call({ to: d.vault, data: vaultCall('forcedDeadline', [forcedIndex]) }, head.number)));
    view.account = {
      address: who,
      balance: big(decodeFunctionResult({ abi: TOKEN_ABI, functionName: 'balanceOf', data: raw[base] })),
      allowance: big(decodeFunctionResult({ abi: TOKEN_ABI, functionName: 'allowance', data: raw[base + 1] })),
      hasDeposited: vaultResult('hasDeposited', raw[base + 2]) as boolean,
      forcedIndex,
      forcedDeadline,
      forcedOpen: forcedIndex !== null && forcedIndex >= view.latest.nextForcedIndex,
      escapePayout: payoutOf(vaultResult('payouts', raw[base + 4])),
    };
  }
  return view;
}

function payoutOf(v: unknown): Payout {
  const p = v as readonly [Hex, bigint, bigint, bigint];
  return { recipient: p[0].toLowerCase() as Hex, total: big(p[1]), paid: big(p[2]), readyAt: big(p[3]) };
}

/** `withdrawalKey(id)` / `escapeKey(account)`: the formulas of the vault's own pure functions (checked on anvil). */
export const withdrawalKey = (id: bigint): Hex => keccak256(encodeAbiParameters([{ type: 'bytes32' }, { type: 'uint256' }], [stringToHex('W', { size: 32 }), id]));
export const escapeKey = (account: Hex): Hex => keccak256(encodeAbiParameters([{ type: 'bytes32' }, { type: 'address' }], [stringToHex('E', { size: 32 }), account]));

export async function readPayout(l2: L2, d: Deployment, key: Hex, at: bigint | 'latest' = 'latest'): Promise<Payout> {
  return payoutOf(vaultResult('payouts', await l2.call({ to: d.vault, data: vaultCall('payouts', [key]) }, at)));
}

/* ---------------------------------------------------------------- states in words */

export type VaultPhase = 'normal' | 'paused' | 'escapeDue' | 'escapeUnproven' | 'escape';
export function phaseOf(v: VaultView): VaultPhase {
  if (v.escapeAt !== 0n) return v.systemSharesProven ? 'escape' : 'escapeUnproven';
  if (v.escapeReason !== 0) return 'escapeDue';
  return v.paused ? 'paused' : 'normal';
}

/** Seconds until escape becomes due by silence, or null when no checkpoint was ever accepted (VAULT.md R395-24). */
export function silenceLeft(v: VaultView): bigint | null {
  if (v.lastCheckpointAt === 0n || v.escapeAt !== 0n) return null;
  const due = v.lastCheckpointAt + v.escapeDelay;
  return due > v.now ? due - v.now : 0n;
}

export type PayoutState = 'none' | 'waiting' | 'payable' | 'partly' | 'paid';
export function payoutState(p: Payout, now: bigint): PayoutState {
  if (p.total === 0n) return 'none';
  if (p.paid >= p.total) return 'paid';
  if (now < p.readyAt) return 'waiting';
  return p.paid > 0n ? 'partly' : 'payable';
}

/** Why a deposit of `amount` cannot be sent now; empty when it can. `amount` in base units. */
export function depositProblems(v: VaultView, amount: bigint | null): string[] {
  const out: string[] = [];
  if (v.escapeAt !== 0n) out.push('The vault is in escape mode: it accepts no deposits, for good.');
  else if (v.paused) out.push('The vault is paused by a guardian: deposits resume when the pause ends.');
  if (!v.account) out.push('Connect a wallet first.');
  if (amount === null || amount <= 0n) return [...out, 'Enter an amount.'];
  if (amount > MAX_U64) out.push('The amount is too large.');
  if (amount < v.minDeposit) out.push(`The vault's minimum deposit is ${usdc(v.minDeposit)}.`);
  if (v.custody + amount > v.cap) out.push(`The vault is capped at ${usdc(v.cap)} and holds ${usdc(v.custody)}: at most ${usdc(v.cap > v.custody ? v.cap - v.custody : 0n)} can be deposited now.`);
  if (v.account && amount > v.account.balance) out.push(`This wallet holds ${usdc(v.account.balance)} on the Bloch L2.`);
  return out;
}

/** Why a forced request cannot be posted now; empty when it can. */
export function forcedProblems(v: VaultView, kind: ForcedKind, arg: bigint): string[] {
  const out: string[] = [];
  if (v.escapeAt !== 0n) out.push('The vault is in escape mode: claim your balance below instead.');
  if (!v.account) return [...out, 'Connect a wallet first.'];
  if (!v.account.hasDeposited) out.push('Only an account that deposited through the vault can post a request.');
  if (v.account.forcedOpen) out.push('This account already has a request open. One request at a time.');
  if (kind !== 'withdraw' && arg !== 0n) out.push('Only a withdrawal request carries an amount.');
  if (arg < 0n || arg > MAX_U64) out.push('The amount is out of range.');
  return out;
}

/**
 * What an escape claim of `escapeValue` registers: floor(escapeValue * escapePayable / senior), senior =
 * totalEscape - subordinated (VAULT.md 7, the vault's own formula and rounding). Null before the system shares are
 * proven (the ratio is not fixed yet) or when nothing is senior.
 */
export function escapeAmount(v: VaultView, escapeValue: bigint): bigint | null {
  if (v.escapeAt === 0n || !v.systemSharesProven) return null;
  const senior = v.latest.totalEscape - v.subordinatedTotal;
  if (senior <= 0n) return null;
  return (escapeValue * v.escapePayable) / senior;
}

/* ---------------------------------------------------------------- transactions */

export type TxKind = 'approve' | 'deposit' | 'requestForced' | 'withdraw' | 'release' | 'triggerEscape' | 'proveSystemShares' | 'claimEscape' | 'refundDeposit';
export type Tx = { kind: TxKind; to: Hex; data: Hex };
export type TxInput =
  | { kind: 'approve'; amount: bigint }
  | { kind: 'deposit'; amount: bigint; account: Hex }
  | { kind: 'requestForced'; forced: ForcedKind; arg: bigint }
  | { kind: 'withdraw'; seq: bigint; record: WithdrawalRecord; siblings: readonly Hex[]; want: bigint }
  | { kind: 'release'; key: Hex; want: bigint }
  | { kind: 'triggerEscape' }
  | { kind: 'proveSystemShares'; insurance: AccountLeaf; insuranceSiblings: readonly Hex[]; backstop: AccountLeaf; backstopSiblings: readonly Hex[] }
  | { kind: 'claimEscape'; leaf: AccountLeaf; siblings: readonly Hex[]; want: bigint }
  | { kind: 'refundDeposit'; index: bigint; account: Hex; amount: bigint };

const u64 = (v: bigint, what: string) => {
  if (v < 0n || v > MAX_U64) throw new RangeError(`${what} is out of range`);
  return v;
};
const leafTuple = (a: AccountLeaf) => ({ account: a.account, index: a.index, balance: a.balance, escapeValue: a.escapeValue, positionsHash: a.positionsHash });

export function buildTx(d: Deployment, input: TxInput): Tx {
  switch (input.kind) {
    case 'approve':
      return { kind: input.kind, to: d.token, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'approve', args: [d.vault, u64(input.amount, 'amount')] }) };
    case 'deposit':
      if (input.amount <= 0n) throw new RangeError('amount must be positive');
      return { kind: input.kind, to: d.vault, data: vaultCall('deposit', [u64(input.amount, 'amount'), input.account]) };
    case 'requestForced': {
      const kind = FORCED_KINDS.indexOf(input.forced);
      if (kind < 0) throw new RangeError('unknown request kind');
      if (input.forced !== 'withdraw' && input.arg !== 0n) throw new RangeError('only a withdrawal request carries an amount');
      return { kind: input.kind, to: d.vault, data: vaultCall('requestForced', [kind, u64(input.arg, 'amount')]) };
    }
    case 'withdraw':
      return { kind: input.kind, to: d.vault, data: vaultCall('withdraw', [u64(input.seq, 'seq'), { ...input.record }, [...input.siblings], u64(input.want, 'want')]) };
    case 'release':
      return { kind: input.kind, to: d.vault, data: vaultCall('release', [input.key, u64(input.want, 'want')]) };
    case 'triggerEscape':
      return { kind: input.kind, to: d.vault, data: vaultCall('triggerEscape') };
    case 'proveSystemShares':
      return { kind: input.kind, to: d.vault, data: vaultCall('proveSystemShares', [leafTuple(input.insurance), [...input.insuranceSiblings], leafTuple(input.backstop), [...input.backstopSiblings]]) };
    case 'claimEscape':
      return { kind: input.kind, to: d.vault, data: vaultCall('claimEscape', [leafTuple(input.leaf), [...input.siblings], u64(input.want, 'want')]) };
    case 'refundDeposit':
      return { kind: input.kind, to: d.vault, data: vaultCall('refundDeposit', [u64(input.index, 'index'), input.account, u64(input.amount, 'amount')]) };
  }
}

export type TxDescription = { title: string; rows: { label: string; value: string; emphasis?: boolean }[]; notes: string[] };
export class UnknownTx extends Error {}
const FORCED_WORDS: Record<ForcedKind, string> = { cancelAll: 'Cancel all my open orders', closeAll: 'Close all my positions', withdraw: 'Withdraw from my trading account' };

/**
 * Describes a transaction FROM ITS CALLDATA (decoded with the pinned ABIs). Throws UnknownTx for a target that is not
 * the configured vault or token, or a function outside the closed list: such a transaction is never shown or sent.
 */
export function describeTx(d: Deployment, tx: { to: string; data: Hex }): TxDescription {
  const to = tx.to.toLowerCase();
  const contract = { label: 'Contract', value: `${to === d.token ? 'bUSDC token' : 'PerpsVault'} ${to}` };
  const chain = { label: 'Network', value: `Bloch L2 (chain ${d.chainId})` };
  if (to === d.token) {
    let f;
    try {
      f = decodeFunctionData({ abi: TOKEN_ABI, data: tx.data });
    } catch {
      throw new UnknownTx('unknown token call');
    }
    if (f.functionName !== 'approve') throw new UnknownTx('unknown token call');
    const [spender, amount] = f.args as readonly [Hex, bigint];
    if (spender.toLowerCase() !== d.vault) throw new UnknownTx('approval to another spender than the vault');
    if (amount > MAX_U64) throw new UnknownTx('unlimited approval');
    return {
      title: `Allow the vault to take ${usdc(amount)} from this wallet`,
      rows: [{ label: 'Allowance', value: usdc(amount), emphasis: true }, { label: 'Spender', value: `PerpsVault ${d.vault}` }, contract, chain],
      notes: ['This moves nothing yet. It lets the vault take exactly this amount, once you send the deposit.', 'The allowance is for this amount only, never unlimited.'],
    };
  }
  if (to !== d.vault) throw new UnknownTx('unknown contract');
  let f;
  try {
    f = decodeFunctionData({ abi: VAULT_ABI, data: tx.data });
  } catch {
    throw new UnknownTx('unknown vault call');
  }
  const a = (f.args ?? []) as readonly unknown[];
  switch (f.functionName) {
    case 'deposit':
      return {
        title: `Deposit ${usdc(a[0] as bigint)} into the exchange vault`,
        rows: [{ label: 'Amount', value: usdc(a[0] as bigint), emphasis: true }, { label: 'Credited to trading account', value: String(a[1]).toLowerCase(), emphasis: true }, contract, chain],
        notes: ['This moves bUSDC from your wallet into the vault. The exchange credits it after the L2 block is final (minutes).', 'Funds in the vault leave only by a withdrawal the validators certified, or by the escape path. There is no admin key.'],
      };
    case 'requestForced': {
      const kind = FORCED_KINDS[Number(a[0])];
      if (!kind) throw new UnknownTx('unknown request kind');
      const arg = a[1] as bigint;
      return {
        title: `Post a request on the L2 the exchange must process: ${FORCED_WORDS[kind].toLowerCase()}`,
        rows: [
          { label: 'Request', value: FORCED_WORDS[kind], emphasis: true },
          ...(kind === 'withdraw' ? [{ label: 'Amount', value: arg === 0n ? 'All free collateral' : usdc(arg), emphasis: true }, { label: 'Paid to', value: 'This wallet (the account itself), by the normal withdrawal path' }] : []),
          contract,
          chain,
        ],
        notes: ['The exchange must process this before the deadline the vault sets. If it does not, anyone can switch the vault to escape mode and every user exits with their last proven balance.', 'One request can be open per account.'],
      };
    }
    case 'withdraw': {
      const w = a[1] as { withdrawalId: bigint; account: Hex; recipient: Hex; amount: bigint; height: bigint };
      return {
        title: `Collect withdrawal #${w.withdrawalId}: ${usdc(w.amount)} to ${w.recipient.toLowerCase()}`,
        rows: [
          { label: 'Amount', value: usdc(w.amount), emphasis: true },
          { label: 'Paid to', value: w.recipient.toLowerCase(), emphasis: true },
          { label: 'From trading account', value: w.account.toLowerCase() },
          { label: 'Withdrawal', value: `#${w.withdrawalId}, certified in vault checkpoint ${a[0]}` },
          { label: 'Pay now, up to', value: usdc(a[3] as bigint) },
          contract,
          chain,
        ],
        notes: ['The vault pays the recipient named in the certified record, whoever sends this transaction.', 'A large amount is registered now and paid after the vault\'s delay.'],
      };
    }
    case 'release':
      return { title: `Collect up to ${usdc(a[1] as bigint)} of a registered payout`, rows: [{ label: 'Pay now, up to', value: usdc(a[1] as bigint), emphasis: true }, { label: 'Payout key', value: String(a[0]) }, contract, chain], notes: ['The vault pays the recipient it registered, whoever sends this transaction.'] };
    case 'triggerEscape':
      return { title: 'Switch the vault to escape mode', rows: [contract, chain], notes: ['ONE WAY: the exchange stops for good. Deposits, requests and checkpoints end; every account then claims its last proven balance from the vault.', 'The vault accepts this only when the exchange missed a deadline.'] };
    case 'proveSystemShares':
      return { title: 'Prove the insurance and backstop balances to the vault', rows: [contract, chain], notes: ['Needed once, by anyone, before accounts can claim in escape mode: it fixes that users are paid before the insurance fund and the backstop.', 'It pays nothing to you and takes nothing from you.'] };
    case 'claimEscape': {
      const leaf = a[0] as { account: Hex; escapeValue: bigint };
      return {
        title: `Claim the escape balance of ${leaf.account.toLowerCase()}`,
        rows: [{ label: 'Proven value', value: usdc(leaf.escapeValue), emphasis: true }, { label: 'Paid to', value: leaf.account.toLowerCase(), emphasis: true }, { label: 'Pay now, up to', value: usdc(a[2] as bigint) }, contract, chain],
        notes: ['The vault pays the account itself. If the vault holds less than all users are owed, every claim is reduced in the same proportion.'],
      };
    }
    case 'refundDeposit':
      return { title: `Refund deposit #${a[0]}: ${usdc(a[2] as bigint)} to ${String(a[1]).toLowerCase()}`, rows: [{ label: 'Amount', value: usdc(a[2] as bigint), emphasis: true }, { label: 'Paid to', value: String(a[1]).toLowerCase(), emphasis: true }, contract, chain], notes: ['In escape mode a deposit the exchange never credited is returned in full from the vault\'s own records.'] };
    default:
      throw new UnknownTx('a vault function this interface never calls');
  }
}

/** A revert in words. */
export function revertWords(data: Hex | null): string {
  if (!data) return 'The Bloch L2 would reject this transaction.';
  try {
    const e = decodeErrorResult({ abi: VAULT_ABI, data });
    return `The vault would reject this: ${REVERT_WORDS[e.errorName] ?? e.errorName}.`;
  } catch {
    return 'The transaction would be rejected (the token or the vault refused it).';
  }
}

export class TxRefused extends Error {}
export type WalletRequest = (args: { method: string; params?: unknown[] }) => Promise<unknown>;

/** Runs the call from `from` on the L2 endpoint; throws TxRefused with words when it would revert. */
export async function simulate(l2: L2, from: Hex, tx: Tx): Promise<void> {
  try {
    await l2.call({ to: tx.to, data: tx.data, from });
  } catch (e) {
    if (e instanceof L2Error && e.revertData !== null) throw new TxRefused(revertWords(e.revertData));
    if (e instanceof L2Error && e.message === 'The call would revert.') throw new TxRefused(revertWords(null));
    throw new TxRefused(`${e instanceof L2Error ? e.message : 'The Bloch L2 could not be read.'} Nothing was sent.`);
  }
}

/**
 * Sends `tx` through the wallet. Order: the transaction must describe (closed list, pinned targets); the wallet must
 * be on the L2 and on `from`; the call must not revert in simulation; only then the wallet is asked. Returns the hash.
 */
export async function sendTx(wallet: WalletRequest, l2: L2, d: Deployment, from: string, tx: Tx): Promise<Hex> {
  try {
    describeTx(d, tx);
  } catch {
    throw new TxRefused('This interface cannot describe that transaction, so it will not send it.');
  }
  const sender = from.toLowerCase() as Hex;
  const chain = await wallet({ method: 'eth_chainId' });
  if (typeof chain !== 'string' || !/^0x[0-9a-fA-F]+$/.test(chain) || BigInt(chain) !== BigInt(d.chainId)) throw new TxRefused(`The wallet is not on the Bloch L2 (chain ${d.chainId}). Switch network and try again. Nothing was sent.`);
  const accounts = await wallet({ method: 'eth_accounts' });
  if (!Array.isArray(accounts) || typeof accounts[0] !== 'string' || accounts[0].toLowerCase() !== sender) throw new TxRefused('The wallet account changed. Review again. Nothing was sent.');
  await simulate(l2, sender, tx);
  let gas: bigint;
  try {
    gas = gasLimit(await l2.estimateGas({ from: sender, to: tx.to, data: tx.data }));
  } catch (e) {
    throw new TxRefused(e instanceof TxRefused ? e.message : 'The gas this transaction needs could not be estimated. Nothing was sent.');
  }
  const hash = await wallet({ method: 'eth_sendTransaction', params: [{ from: sender, to: tx.to, data: tx.data, value: '0x0', gas: `0x${gas.toString(16)}` }] });
  if (typeof hash !== 'string' || !/^0x[0-9a-fA-F]{64}$/.test(hash)) throw new TxRefused('The wallet returned no transaction hash. Check the wallet before trying again.');
  return hash.toLowerCase() as Hex;
}

/** The Bloch L2's per-transaction gas cap (docs/VAULT.md 9, L2 go-live K8). */
export const L2_TX_GAS_CAP = 15_000_000n;
/**
 * F434-1. The gas limit the page puts on a transaction: 1.5 times the node's estimate plus 40,000, at most the L2's
 * per-transaction cap. Why the page sets it instead of leaving it to the wallet: the vault's payout path calls the
 * token and re-reads balances under the 63/64 forwarding rule, and its cost moves with the block time (bucket refill,
 * ready times). An estimate taken at the exact minimum for one block ran out of gas in the next on a local anvil
 * (estimate 168,094; used 168,078; status 0). Unused gas is not charged. An estimate above the cap is refused.
 */
export function gasLimit(estimate: bigint): bigint {
  if (estimate <= 0n) throw new TxRefused('The gas estimate is unusable. Nothing was sent.');
  if (estimate > L2_TX_GAS_CAP) throw new TxRefused('This transaction needs more gas than a Bloch L2 transaction may use. Nothing was sent.');
  const padded = (estimate * 3n) / 2n + 40_000n;
  return padded > L2_TX_GAS_CAP ? L2_TX_GAS_CAP : padded;
}

/** Polls for the receipt. Resolves null when `timeoutMs` passes (the transaction may still land: say so). */
export async function waitReceipt(l2: L2, hash: Hex, options: { timeoutMs?: number; pollMs?: number; sleep?: (ms: number) => Promise<void> } = {}): Promise<Receipt | null> {
  const { timeoutMs = 180_000, pollMs = 2_000, sleep = (ms) => new Promise<void>((r) => setTimeout(r, ms)) } = options;
  for (let waited = 0; ; waited += pollMs) {
    const r = await l2.receipt(hash);
    if (r) return r;
    if (waited >= timeoutMs) return null;
    await sleep(pollMs);
  }
}

/* ---------------------------------------------------------------- events */

const DEPOSIT_TOPIC = toEventSelector('Deposit(uint64,address,address,uint64,bytes32)');
const FORCED_TOPIC = toEventSelector('ForcedRequest(uint64,address,uint8,uint64,uint64,bytes32)');
const topicAddress = (a: Hex): Hex => `0x${'00'.repeat(12)}${a.slice(2).toLowerCase()}`;
export type VaultDeposit = { index: bigint; account: Hex; sender: Hex; amount: bigint; block: bigint; txHash: Hex };
export type VaultForced = { index: bigint; account: Hex; kind: ForcedKind; deadline: bigint; arg: bigint; block: bigint; txHash: Hex };

function depositOf(d: Deployment, log: Receipt['logs'][number]): VaultDeposit | null {
  if (log.address !== d.vault || log.topics[0] !== DEPOSIT_TOPIC) return null;
  const e = decodeEventLog({ abi: VAULT_ABI, eventName: 'Deposit', topics: log.topics as [Hex, ...Hex[]], data: log.data }) as unknown as { args: { index: bigint; account: Hex; sender: Hex; amount: bigint } };
  return { index: e.args.index, account: e.args.account.toLowerCase() as Hex, sender: e.args.sender.toLowerCase() as Hex, amount: e.args.amount, block: log.blockNumber, txHash: log.transactionHash };
}
function forcedOf(d: Deployment, log: Receipt['logs'][number]): VaultForced | null {
  if (log.address !== d.vault || log.topics[0] !== FORCED_TOPIC) return null;
  const e = decodeEventLog({ abi: VAULT_ABI, eventName: 'ForcedRequest', topics: log.topics as [Hex, ...Hex[]], data: log.data }) as unknown as { args: { index: bigint; account: Hex; kind: number; deadline: bigint; arg: bigint } };
  const kind = FORCED_KINDS[e.args.kind];
  if (!kind) return null;
  return { index: e.args.index, account: e.args.account.toLowerCase() as Hex, kind, deadline: e.args.deadline, arg: e.args.arg, block: log.blockNumber, txHash: log.transactionHash };
}

/** The Deposit event of the vault in a receipt (the vault's own record of what it took and for whom). */
export const depositInReceipt = (d: Deployment, r: Receipt): VaultDeposit | null => r.logs.map((l) => depositOf(d, l)).find((x) => x !== null) ?? null;
export const forcedInReceipt = (d: Deployment, r: Receipt): VaultForced | null => r.logs.map((l) => forcedOf(d, l)).find((x) => x !== null) ?? null;

/** The account's deposits as the VAULT recorded them (L2 logs), independent of the exchange API. Oldest first. */
export async function depositsOf(l2: L2, d: Deployment, account: Hex, fromBlock: bigint, toBlock: bigint): Promise<VaultDeposit[]> {
  const logs = await l2.logs({ address: d.vault, topics: [DEPOSIT_TOPIC, null, topicAddress(account)], fromBlock, toBlock });
  return logs.map((l) => depositOf(d, l)).filter((x): x is VaultDeposit => x !== null && x.account === account.toLowerCase());
}
export async function forcedRequestsOf(l2: L2, d: Deployment, account: Hex, fromBlock: bigint, toBlock: bigint): Promise<VaultForced[]> {
  const logs = await l2.logs({ address: d.vault, topics: [FORCED_TOPIC, null, topicAddress(account)], fromBlock, toBlock });
  return logs.map((l) => forcedOf(d, l)).filter((x): x is VaultForced => x !== null && x.account === account.toLowerCase());
}

/* ---------------------------------------------------------------- claims */

export class ClaimRefused extends Error {}
export type WithdrawalClaim = { seq: bigint; record: WithdrawalRecord; siblings: Hex[]; payout: Payout };
/** How many of the newest vault checkpoints are searched for the one that certified a record. */
export const SEQ_SEARCH = 64n;

/**
 * Checks a withdrawal record and its proof against the VAULT (not the API): the record is hashed here, walked to a
 * root, and that root must be the withdrawals root of an accepted checkpoint that covers the record's id. Returns the
 * checkpoint sequence to claim against and the payout the vault already holds for it (total 0 when unregistered).
 * `expect` binds the record to what the user asked for: account and, when known, recipient and amount.
 */
export async function checkWithdrawalClaim(
  l2: L2,
  d: Deployment,
  v: VaultView,
  record: WithdrawalRecord,
  siblings: readonly string[],
  expect: { account: string; recipient?: string; amount?: bigint },
): Promise<WithdrawalClaim> {
  if (record.account.toLowerCase() !== expect.account.toLowerCase()) throw new ClaimRefused('The record names another account than the connected wallet.');
  if (expect.recipient !== undefined && record.recipient.toLowerCase() !== expect.recipient.toLowerCase()) throw new ClaimRefused('The record pays another recipient than the one you signed.');
  if (expect.amount !== undefined && record.amount !== expect.amount) throw new ClaimRefused('The record carries another amount than the exchange reported for this withdrawal.');
  if (record.amount <= 0n) throw new ClaimRefused('The record carries no amount.');
  let root: Hex;
  try {
    root = rootFromProof(withdrawalLeaf(record), record.withdrawalId, siblings);
  } catch (e) {
    throw new ClaimRefused(`The proof is malformed (${e instanceof Error ? e.message : 'unreadable'}).`);
  }
  const payout = await readPayout(l2, d, withdrawalKey(record.withdrawalId), v.block);
  const newest = v.checkpointCount;
  const oldest = newest > SEQ_SEARCH ? newest - SEQ_SEARCH + 1n : 1n;
  const seqs: bigint[] = [];
  for (let s = newest; s >= oldest && s >= 1n; s--) seqs.push(s);
  const answers = await l2.calls(seqs.flatMap((s) => [{ to: d.vault, data: vaultCall('withdrawalsRootAt', [s]) }, { to: d.vault, data: vaultCall('withdrawalCountAt', [s]) }]), v.block);
  for (let i = 0; i < seqs.length; i++) {
    const rootAt = String(vaultResult('withdrawalsRootAt', answers[2 * i])).toLowerCase();
    const countAt = big(vaultResult('withdrawalCountAt', answers[2 * i + 1]));
    if (rootAt === root && record.withdrawalId < countAt) return { seq: seqs[i], record, siblings: siblings.map((s) => s.toLowerCase() as Hex), payout };
  }
  throw new ClaimRefused(newest === 0n ? 'The vault has accepted no checkpoint yet, so it cannot pay this.' : 'The vault holds no checkpoint that certifies this record with this proof. It may not have reached the vault yet; if it never does, use the exit below.');
}

/** Checks an escape leaf and proof against the account root of the vault's latest checkpoint. */
export function checkEscapeClaim(v: VaultView, leaf: AccountLeaf, siblings: readonly string[], account: string): void {
  if (leaf.account.toLowerCase() !== account.toLowerCase()) throw new ClaimRefused('The proof is for another account.');
  let root: Hex;
  try {
    root = rootFromProof(accountLeaf(leaf), BigInt(leaf.index), siblings);
  } catch (e) {
    throw new ClaimRefused(`The proof is malformed (${e instanceof Error ? e.message : 'unreadable'}).`);
  }
  if (root !== v.latest.accountsRoot.toLowerCase()) throw new ClaimRefused('The proof does not match the last checkpoint the vault accepted. Ask for a proof against that checkpoint.');
}
