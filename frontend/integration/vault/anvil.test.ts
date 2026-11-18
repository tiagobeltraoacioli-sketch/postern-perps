/**
 * The page's vault module (src/vault) against the REAL PerpsVault bytecode on a local anvil, chain 700770.
 * UNAUDITED prototype; loopback only; committee and guardian keys generated here at run time and never written.
 *
 * What the engine and the node will supply later (checkpoints, records, proofs) is produced here by a stand-in that
 * follows docs/VAULT.md 5 with src/vault/merkle.ts; the vault itself decides what it accepts. The scenario is the
 * user's whole custody life: deposit, a certified withdrawal collected with a proof, a second deposit the exchange
 * never credits, a forced request the exchange ignores, escape, the system-share proof, the escape claim and the
 * refund. Every transaction goes through `sendTx` exactly as the page sends it (describe, wallet checks, simulation).
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { beforeAll, describe, expect, it } from 'vitest';
import { decodeFunctionResult, encodeDeployData, encodeFunctionData, type Abi, type Hex } from 'viem';
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';
import { L2 } from '../../src/vault/l2';
import { accountLeaf, SparseTree, withdrawalLeaf, type AccountLeaf, type WithdrawalRecord } from '../../src/vault/merkle';
import { TOKEN_ABI, VAULT_ABI } from '../../src/vault/abi';
import {
  buildTx,
  checkEscapeClaim,
  checkWithdrawalClaim,
  ClaimRefused,
  depositInReceipt,
  depositProblems,
  depositsOf,
  escapeAmount,
  escapeKey,
  forcedInReceipt,
  forcedProblems,
  payoutState,
  phaseOf,
  readPayout,
  readVault,
  sendTx,
  simulate,
  TxRefused,
  waitReceipt,
  withdrawalKey,
  type Deployment,
  type Tx,
  type WalletRequest,
} from '../../src/vault/vault';

const RPC = process.env.PERPS_ANVIL_RPC ?? '';
const OUT = process.env.PERPS_FORGE_OUT ?? '';
const CHAIN = 700770;
const U = 1_000_000n; // 1 bUSDC
const OWNER = '0x775a993f09bd80aecf8f8c2ade0c84158e3a0544' as Hex;
const evidence: { check: string; ok: boolean; detail?: unknown }[] = [];
const note = (check: string, detail?: unknown) => evidence.push({ check, ok: true, detail: JSON.parse(JSON.stringify(detail ?? null, (_, v) => (typeof v === 'bigint' ? v.toString() : v))) });

let id = 0;
async function rpc(method: string, params: unknown[] = []): Promise<unknown> {
  const r = await fetch(RPC, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: ++id, method, params }) });
  const j = (await r.json()) as { result?: unknown; error?: { message: string } };
  if (j.error) throw new Error(`${method}: ${j.error.message}`);
  return j.result;
}
const artifact = (path: string) => JSON.parse(readFileSync(resolve(OUT, path), 'utf8')) as { abi: Abi; bytecode: { object: Hex } };
async function mined(hash: Hex) {
  const r = (await rpc('eth_getTransactionReceipt', [hash])) as { status: string; contractAddress: Hex | null };
  if (r.status !== '0x1') throw new Error('setup transaction reverted');
  return r;
}
async function deploy(from: Hex, a: { abi: Abi; bytecode: { object: Hex } }, args: unknown[]): Promise<Hex> {
  const hash = (await rpc('eth_sendTransaction', [{ from, data: encodeDeployData({ abi: a.abi, bytecode: a.bytecode.object, args }) }])) as Hex;
  return ((await mined(hash)).contractAddress as Hex).toLowerCase() as Hex;
}
async function send(from: Hex, to: Hex, data: Hex) {
  await mined((await rpc('eth_sendTransaction', [{ from, to, data }])) as Hex);
}

let accounts: Hex[];
let user: Hex;
let lp: Hex;
let relayer: Hex;
let d: Deployment;
let l2: L2;
let tokenAbi: Abi;
const committee = Array.from({ length: 7 }, () => privateKeyToAccount(generatePrivateKey())).sort((a, b) => (a.address.toLowerCase() < b.address.toLowerCase() ? -1 : 1));
/** Every transaction object the page handed to a wallet (F434-1: each must carry the page's own gas limit). */
const sent: { gas?: string }[] = [];
/** The wallet of `who`: anvil's unlocked account, behind the EIP-1193 request shape the page uses. */
const walletOf =
  (who: Hex, chainHex = `0x${CHAIN.toString(16)}`): WalletRequest =>
  async ({ method, params }) => {
    if (method === 'eth_chainId') return chainHex;
    if (method === 'eth_accounts') return [who];
    if (method === 'eth_sendTransaction') {
      sent.push(params![0] as { gas?: string });
      return rpc(method, params);
    }
    throw new Error(`unsupported ${method}`);
  };
async function pageSend(who: Hex, tx: Tx) {
  const hash = await sendTx(walletOf(who), l2, d, who, tx);
  const receipt = await waitReceipt(l2, hash, { pollMs: 50 });
  expect(receipt?.status).toBe('success');
  return receipt!;
}
const balanceOf = async (who: Hex) => decodeFunctionResult({ abi: TOKEN_ABI, functionName: 'balanceOf', data: await l2.call({ to: d.token, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'balanceOf', args: [who] }) }) }) as bigint;
const vaultView = async (name: string, args: unknown[] = []) => decodeFunctionResult({ abi: VAULT_ABI, functionName: name, data: await l2.call({ to: d.vault, data: encodeFunctionData({ abi: VAULT_ABI, functionName: name, args } as never) }) } as never) as unknown;

/* ---- the engine stand-in: account tree, withdrawal tree, a checkpoint signed by 5 of the 7 ---- */
const keyless = (n: number) => `0x${n.toString(16).padStart(40, '0')}` as Hex;
const POSITIONS = `0x${'00'.repeat(32)}` as Hex;
let leaves: AccountLeaf[] = [];
let records: WithdrawalRecord[] = [];
let height = 0n;
const accountTree = () => new SparseTree(leaves.map(accountLeaf));
const withdrawalTree = () => new SparseTree(records.map(withdrawalLeaf));
const CHECKPOINT_TYPES = {
  Checkpoint: [
    { name: 'height', type: 'uint64' },
    { name: 'time', type: 'uint64' },
    { name: 'logHash', type: 'bytes32' },
    { name: 'accountsRoot', type: 'bytes32' },
    { name: 'marketsRoot', type: 'bytes32' },
    { name: 'withdrawalsRoot', type: 'bytes32' },
    { name: 'nextDepositIndex', type: 'uint64' },
    { name: 'nextForcedIndex', type: 'uint64' },
    { name: 'nextOrderId', type: 'uint64' },
    { name: 'totalEscape', type: 'uint128' },
    { name: 'dust', type: 'uint128' },
    { name: 'auxHash', type: 'bytes32' },
    { name: 'depositsHead', type: 'bytes32' },
    { name: 'forcedHead', type: 'bytes32' },
    { name: 'withdrawalCount', type: 'uint64' },
    { name: 'withdrawalsTotal', type: 'uint64' },
  ],
} as const;
async function checkpoint(nextDepositIndex: bigint, nextForcedIndex: bigint) {
  height += 10n;
  const cp = {
    height,
    time: 1_800_000_000_000n + height,
    logHash: POSITIONS,
    accountsRoot: accountTree().root,
    marketsRoot: POSITIONS,
    withdrawalsRoot: withdrawalTree().root,
    nextDepositIndex,
    nextForcedIndex,
    nextOrderId: 1n,
    totalEscape: leaves.reduce((s, l) => s + l.escapeValue, 0n),
    dust: 0n,
    auxHash: POSITIONS,
    depositsHead: (await vaultView('depositHead', [nextDepositIndex])) as Hex,
    forcedHead: (await vaultView('forcedHead', [nextForcedIndex])) as Hex,
    withdrawalCount: BigInt(records.length),
    withdrawalsTotal: records.reduce((s, r) => s + r.amount, 0n),
  };
  const domain = { name: 'PosternPerpsVault', version: '1', chainId: CHAIN, verifyingContract: d.vault };
  const signatures = await Promise.all(committee.slice(0, 5).map((v) => v.signTypedData({ domain, types: CHECKPOINT_TYPES, primaryType: 'Checkpoint', message: cp })));
  await send(relayer, d.vault, encodeFunctionData({ abi: VAULT_ABI, functionName: 'submitCheckpoint', args: [cp, signatures] } as never));
}

describe.skipIf(!RPC || !OUT)('vault module against PerpsVault on anvil (chain 700770)', () => {
  beforeAll(async () => {
    accounts = ((await rpc('eth_accounts')) as Hex[]).map((a) => a.toLowerCase() as Hex);
    [, user, lp, relayer] = accounts;
    const token = artifact('Base.sol/MockUSDC.json');
    const vault = artifact('PerpsVault.sol/PerpsVault.json');
    tokenAbi = token.abi;
    const tokenAddress = await deploy(accounts[0], token, []);
    const guardians = accounts.slice(6, 9).sort();
    const config = {
      token: tokenAddress,
      feeRecipient: OWNER,
      validators: committee.map((v) => v.address),
      threshold: 5,
      unpauseThreshold: 6,
      cap: 1_000_000n * U,
      minDeposit: 10n * U,
      windowLimit: 250_000n * U,
      windowSeconds: 86_400n,
      largeThreshold: 50_000n * U,
      largeDelay: 86_400n,
      forcedDelay: 2n * 86_400n,
      escapeDelay: 7n * 86_400n,
      guardians,
      pauseDuration: 2n * 86_400n,
      pauseCooldown: 2n * 86_400n,
    };
    const vaultAddress = await deploy(accounts[0], vault, [config]);
    d = { chainId: CHAIN, vault: vaultAddress, token: tokenAddress };
    l2 = new L2(RPC, CHAIN);
    for (const who of [user, lp]) await send(accounts[0], tokenAddress, encodeFunctionData({ abi: tokenAbi, functionName: 'mint', args: [who, 10_000n * U] }));
    note('deployed', { vault: vaultAddress, token: tokenAddress, committee: committee.map((v) => v.address), threshold: 5 });
  });

  it('the pinned ABI is the ABI of the build under test', () => {
    const built = artifact('PerpsVault.sol/PerpsVault.json').abi.filter((e) => e.type !== 'constructor');
    expect(JSON.stringify(VAULT_ABI)).toBe(JSON.stringify(built));
  });

  it('refuses an endpoint on another chain, and a vault whose token is not the configured one', async () => {
    await expect(new L2(RPC, 1).ready()).rejects.toThrow(/chain 700770, not the Bloch L2 \(chain 1\)/);
    await expect(readVault(l2, { ...d, token: accounts[9] }, user)).rejects.toThrow(/another token/);
  });

  it('reads the view; deposit refusals in words match what the vault would do', async () => {
    const v = await readVault(l2, d, user);
    expect(phaseOf(v)).toBe('normal');
    expect(v.minDeposit).toBe(10n * U);
    expect(v.account?.balance).toBe(10_000n * U);
    expect(depositProblems(v, 5n * U).join(' ')).toMatch(/minimum deposit is 10\.00 bUSDC/);
    expect(depositProblems(v, 20_000n * U).join(' ')).toMatch(/holds 10,000\.00 bUSDC/);
    expect(depositProblems(v, 1_000n * U)).toEqual([]);
    // Without an allowance the deposit itself would revert: the page says so instead of asking the wallet.
    await expect(simulate(l2, user, buildTx(d, { kind: 'deposit', amount: 1_000n * U, account: user }))).rejects.toThrow(TxRefused);
    // Below the minimum the vault's own error is shown in words.
    await send(user, d.token, encodeFunctionData({ abi: TOKEN_ABI, functionName: 'approve', args: [d.vault, 5n * U] }));
    await expect(simulate(l2, user, buildTx(d, { kind: 'deposit', amount: 5n * U, account: user }))).rejects.toThrow(/refuses this deposit/);
  });

  it('approve the exact amount, deposit, and read the vault\'s own Deposit record', async () => {
    await pageSend(user, buildTx(d, { kind: 'approve', amount: 1_000n * U }));
    expect((await readVault(l2, d, user)).account?.allowance).toBe(1_000n * U);
    const receipt = await pageSend(user, buildTx(d, { kind: 'deposit', amount: 1_000n * U, account: user }));
    const dep = depositInReceipt(d, receipt);
    expect(dep).toMatchObject({ index: 0n, account: user, sender: user, amount: 1_000n * U });
    const v = await readVault(l2, d, user);
    expect(v.custody).toBe(1_000n * U);
    expect(v.account).toMatchObject({ allowance: 0n, balance: 9_000n * U, hasDeposited: true });
    await pageSend(lp, buildTx(d, { kind: 'approve', amount: 200n * U }));
    await pageSend(lp, buildTx(d, { kind: 'deposit', amount: 200n * U, account: lp }));
    note('deposit', { user: dep, custody: (await readVault(l2, d, null)).custody });
  });

  it('a certified withdrawal: the proof is checked against the vault, then collected; altered records are refused in the page', async () => {
    leaves = [
      { account: keyless(1), index: 0, balance: 0n, escapeValue: 0n, positionsHash: POSITIONS },
      { account: keyless(2), index: 1, balance: 60n * U, escapeValue: 60n * U, positionsHash: POSITIONS },
      { account: keyless(3), index: 2, balance: 25n * U, escapeValue: 25n * U, positionsHash: POSITIONS },
      { account: user, index: 3, balance: 750n * U, escapeValue: 750n * U, positionsHash: POSITIONS },
      { account: lp, index: 4, balance: 115n * U, escapeValue: 115n * U, positionsHash: POSITIONS },
    ];
    const record: WithdrawalRecord = { withdrawalId: 0n, account: user, recipient: user, amount: 250n * U, height: 9n };
    records = [record];
    const proof = withdrawalTree().proof(0);
    let v = await readVault(l2, d, user);
    await expect(checkWithdrawalClaim(l2, d, v, record, proof, { account: user })).rejects.toThrow(/accepted no checkpoint yet/);
    await checkpoint(2n, 0n);
    v = await readVault(l2, d, user);
    expect(v.checkpointCount).toBe(1n);
    // The API cannot swap the recipient, the amount or the proof without the page noticing.
    const thief = accounts[9];
    await expect(checkWithdrawalClaim(l2, d, v, { ...record, recipient: thief }, proof, { account: user })).rejects.toThrow(/holds no checkpoint that certifies/);
    await expect(checkWithdrawalClaim(l2, d, v, { ...record, recipient: thief }, proof, { account: user, recipient: user })).rejects.toThrow(/another recipient/);
    await expect(checkWithdrawalClaim(l2, d, v, { ...record, amount: 251n * U }, proof, { account: user, amount: 250n * U })).rejects.toThrow(/another amount/);
    await expect(checkWithdrawalClaim(l2, d, v, record, proof.slice(1), { account: user })).rejects.toThrow(ClaimRefused);
    await expect(checkWithdrawalClaim(l2, d, v, record, proof, { account: lp })).rejects.toThrow(/another account/);
    const claim = await checkWithdrawalClaim(l2, d, v, record, proof, { account: user, recipient: user, amount: 250n * U });
    expect(claim.seq).toBe(1n);
    expect(payoutState(claim.payout, v.now)).toBe('none');
    expect(withdrawalKey(0n)).toBe(await vaultView('withdrawalKey', [0n]));
    const before = await balanceOf(user);
    // Anyone may send it (here the relayer); the vault pays the recipient of the record.
    await pageSend(relayer, buildTx(d, { kind: 'withdraw', seq: claim.seq, record, siblings: claim.siblings, want: record.amount }));
    expect((await balanceOf(user)) - before).toBe(250n * U);
    const paid = await readPayout(l2, d, withdrawalKey(0n));
    expect(payoutState(paid, v.now)).toBe('paid');
    await expect(simulate(l2, relayer, buildTx(d, { kind: 'withdraw', seq: claim.seq, record, siblings: claim.siblings, want: record.amount }))).rejects.toThrow(/already registered/);
    note('withdrawal collected', { seq: claim.seq, paid });
  });

  it('a forced request the exchange ignores: one open at a time, deadline from the vault', async () => {
    // A third deposit that no checkpoint will ever credit (refunded in escape below).
    await pageSend(user, buildTx(d, { kind: 'approve', amount: 100n * U }));
    await pageSend(user, buildTx(d, { kind: 'deposit', amount: 100n * U, account: user }));
    let v = await readVault(l2, d, user);
    expect(forcedProblems(v, 'withdraw', 0n)).toEqual([]);
    expect(forcedProblems(await readVault(l2, d, accounts[5]), 'cancelAll', 0n).join(' ')).toMatch(/Only an account that deposited/);
    await expect(simulate(l2, accounts[5], buildTx(d, { kind: 'requestForced', forced: 'cancelAll', arg: 0n }))).rejects.toThrow(/refuses this request/);
    const receipt = await pageSend(user, buildTx(d, { kind: 'requestForced', forced: 'withdraw', arg: 0n }));
    const forced = forcedInReceipt(d, receipt)!;
    v = await readVault(l2, d, user);
    expect(forced).toMatchObject({ index: 0n, account: user, kind: 'withdraw', arg: 0n });
    expect(v.account).toMatchObject({ forcedIndex: 0n, forcedOpen: true, forcedDeadline: forced.deadline });
    expect(forced.deadline - v.now).toBe(v.forcedDelay);
    expect(forcedProblems(v, 'closeAll', 0n).join(' ')).toMatch(/already has a request open/);
    await expect(simulate(l2, user, buildTx(d, { kind: 'requestForced', forced: 'closeAll', arg: 0n }))).rejects.toThrow(/refuses this request/);
    expect(phaseOf(v)).toBe('normal');
    await expect(simulate(l2, user, buildTx(d, { kind: 'triggerEscape' }))).rejects.toThrow(/escape is not due/);
    note('forced request', forced);
  });

  it('the deadline passes: escape is due, triggered, system shares proven, the account claims and the deposit is refunded', async () => {
    let v = await readVault(l2, d, user);
    await rpc('evm_increaseTime', [Number(v.forcedDelay) + 1]);
    await rpc('evm_mine');
    v = await readVault(l2, d, user);
    expect(phaseOf(v)).toBe('escapeDue');
    expect(v.escapeReason).toBe(2);
    await pageSend(user, buildTx(d, { kind: 'triggerEscape' }));
    v = await readVault(l2, d, user);
    expect(phaseOf(v)).toBe('escapeUnproven');
    expect(escapeAmount(v, 750n * U)).toBeNull();
    expect(depositProblems(v, 100n * U).join(' ')).toMatch(/escape mode/);
    await expect(simulate(l2, user, buildTx(d, { kind: 'deposit', amount: 100n * U, account: user }))).rejects.toThrow(/escape mode/);
    const tree = accountTree();
    const mine = leaves[3];
    checkEscapeClaim(v, mine, tree.proof(3), user);
    expect(() => checkEscapeClaim(v, { ...mine, escapeValue: 751n * U }, tree.proof(3), user)).toThrow(/does not match the last checkpoint/);
    expect(() => checkEscapeClaim(v, leaves[4], tree.proof(4), user)).toThrow(/another account/);
    await expect(simulate(l2, user, buildTx(d, { kind: 'claimEscape', leaf: mine, siblings: tree.proof(3), want: mine.escapeValue }))).rejects.toThrow(/not ready to pay/);
    await pageSend(relayer, buildTx(d, { kind: 'proveSystemShares', insurance: leaves[1], insuranceSiblings: tree.proof(1), backstop: leaves[2], backstopSiblings: tree.proof(2) }));
    v = await readVault(l2, d, user);
    expect(phaseOf(v)).toBe('escape');
    // Pool = custody 1,050 - the uncredited 100 = 950; users are owed 865, so they are paid in full; the system
    // accounts (85) get the remaining 85, registered to the owner address and held 90 days.
    expect(v.escapePayable).toBe(865n * U);
    const estimate = escapeAmount(v, mine.escapeValue)!;
    expect(estimate).toBe(750n * U);
    expect(escapeKey(user)).toBe(await vaultView('escapeKey', [user]));
    const before = await balanceOf(user);
    await pageSend(user, buildTx(d, { kind: 'claimEscape', leaf: mine, siblings: tree.proof(3), want: estimate }));
    expect((await balanceOf(user)) - before).toBe(estimate);
    v = await readVault(l2, d, user);
    expect(payoutState(v.account!.escapePayout, v.now)).toBe('paid');
    expect(v.account!.escapePayout.total).toBe(estimate);
    // The uncredited deposit, from the vault's own log, refunded in full.
    const head = await l2.head();
    const deposits = await depositsOf(l2, d, user, 0n, head.number);
    expect(deposits.map((x) => [x.index, x.amount])).toEqual([
      [0n, 1_000n * U],
      [2n, 100n * U],
    ]);
    const uncredited = deposits.filter((x) => x.index >= v.latest.nextDepositIndex);
    expect(uncredited).toHaveLength(1);
    const beforeRefund = await balanceOf(user);
    await pageSend(relayer, buildTx(d, { kind: 'refundDeposit', index: uncredited[0].index, account: user, amount: uncredited[0].amount }));
    expect((await balanceOf(user)) - beforeRefund).toBe(100n * U);
    await expect(simulate(l2, relayer, buildTx(d, { kind: 'refundDeposit', index: 0n, account: user, amount: 1_000n * U }))).rejects.toThrow(/cannot be refunded/);
    // The system remainder is registered to the owner address, not payable for 90 days.
    const system = await readPayout(l2, d, escapeKey(keyless(2)));
    expect(system.recipient).toBe(OWNER);
    expect(system.total).toBe(85n * U);
    expect(payoutState(system, v.now)).toBe('waiting');
    // User 10,000 - 1,000 - 100 deposited + 250 withdrawn + 750 escape + 100 refund = everything back.
    expect(await balanceOf(user)).toBe(10_000n * U);
    note('escape', { payable: v.escapePayable, claim: estimate, refund: uncredited[0], systemRemainder: system, userBalance: await balanceOf(user) });
  });

  it('sendTx never asks the wallet for a wrong chain, another account, or a transaction it cannot describe', async () => {
    const asked: string[] = [];
    const spy =
      (inner: WalletRequest): WalletRequest =>
      async (a) => {
        asked.push(a.method);
        return inner(a);
      };
    const tx = buildTx(d, { kind: 'approve', amount: 10n * U });
    await expect(sendTx(spy(walletOf(user, '0x1')), l2, d, user, tx)).rejects.toThrow(/not on the Bloch L2/);
    await expect(sendTx(spy(walletOf(lp)), l2, d, user, tx)).rejects.toThrow(/account changed/);
    await expect(sendTx(spy(walletOf(user)), l2, d, user, { ...tx, to: accounts[9] })).rejects.toThrow(/cannot describe/);
    await expect(sendTx(spy(walletOf(user)), l2, d, user, { ...tx, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'approve', args: [accounts[9], 10n * U] }) })).rejects.toThrow(/cannot describe/);
    await expect(sendTx(spy(walletOf(user)), l2, d, user, { ...tx, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'approve', args: [d.vault, (1n << 256n) - 1n] }) })).rejects.toThrow(/cannot describe/);
    expect(asked).not.toContain('eth_sendTransaction');
    note('sendTx refusals', asked);
  });

  // F434-1: on this anvil a wallet-side estimate for `withdraw` was sometimes exactly too small (estimate 168,094, used
  // 168,078, status 0: an inner call ran out of gas). The page therefore sets the gas limit itself, with a margin.
  it('F434-1: every transaction the page sent carried its own gas limit, at least 1.5 times the estimate', async () => {
    expect(sent.length).toBeGreaterThanOrEqual(10);
    for (const tx of sent) expect(tx.gas).toMatch(/^0x[1-9a-f][0-9a-f]*$/);
    const tx = buildTx(d, { kind: 'approve', amount: 10n * U });
    const estimate = BigInt((await rpc('eth_estimateGas', [{ from: lp, to: tx.to, data: tx.data }])) as string);
    const before = sent.length;
    await pageSend(lp, tx);
    expect(BigInt(sent[before].gas!)).toBeGreaterThanOrEqual((estimate * 3n) / 2n);
    note('gas limit set by the page', { transactions: sent.length });
  });

  it('writes the evidence file', () => {
    const path = process.env.PERPS_EVIDENCE;
    if (path) writeFileSync(path, `${JSON.stringify({ suite: 'frontend vault module vs PerpsVault on anvil (UNAUDITED prototype)', chainId: CHAIN, checks: evidence }, null, 1)}\n`);
    expect(evidence.length).toBeGreaterThanOrEqual(6);
  });
});
