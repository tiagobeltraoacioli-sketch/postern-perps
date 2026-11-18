/** src/vault: what the page will and will not ask a wallet for, with a scripted L2 endpoint. The same module runs
 *  against the real contract in integration/vault/anvil.test.ts. */
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { encodeErrorResult, encodeFunctionData, encodeFunctionResult, type Hex } from 'viem';
import { TOKEN_ABI, VAULT_ABI, VAULT_SOURCE_SHA256, REVERT_WORDS } from '../vault/abi';
import { L2, L2Error, type Fetch } from '../vault/l2';
import {
  buildTx,
  deploymentOf,
  depositProblems,
  describeTx,
  escapeAmount,
  forcedProblems,
  gasLimit,
  L2_TX_GAS_CAP,
  payoutState,
  phaseOf,
  revertWords,
  sendTx,
  silenceLeft,
  TxRefused,
  UnknownTx,
  usdc,
  type Deployment,
  type VaultView,
  type WalletRequest,
} from '../vault/vault';

const VAULT = `0x${'aa'.repeat(20)}` as Hex;
const TOKEN = `0x${'bb'.repeat(20)}` as Hex;
const USER = `0x${'c1'.repeat(20)}` as Hex;
const OTHER = `0x${'d2'.repeat(20)}` as Hex;
const d: Deployment = { chainId: 700770, vault: VAULT, token: TOKEN };
const U = 1_000_000n;
const ZERO32 = `0x${'00'.repeat(32)}` as Hex;

function view(extra: Partial<VaultView> = {}, account: Partial<NonNullable<VaultView['account']>> | null = {}): VaultView {
  return {
    block: 100n,
    now: 1_800_000_000n,
    paused: false,
    pausedUntil: 0n,
    escapeAt: 0n,
    escapeReason: 0,
    systemSharesProven: false,
    escapePayable: 0n,
    subordinatedTotal: 0n,
    minDeposit: 10n * U,
    cap: 1_000_000n * U,
    custody: 400_000n * U,
    largeThreshold: 50_000n * U,
    largeDelay: 86_400n,
    forcedDelay: 172_800n,
    escapeDelay: 604_800n,
    availableToRelease: 250_000n * U,
    checkpointCount: 3n,
    lastCheckpointAt: 1_799_999_000n,
    depositCount: 9n,
    forcedCount: 1n,
    latest: { height: 30n, time: 1n, nextDepositIndex: 9n, nextForcedIndex: 1n, withdrawalCount: 2n, withdrawalsTotal: 500n * U, totalEscape: 1_000n * U, accountsRoot: ZERO32, stateRoot: ZERO32 },
    account:
      account === null
        ? null
        : { address: USER, balance: 5_000n * U, allowance: 0n, hasDeposited: true, forcedIndex: null, forcedDeadline: null, forcedOpen: false, escapePayout: { recipient: USER, total: 0n, paid: 0n, readyAt: 0n }, ...account },
    ...extra,
  };
}

describe('vault ABI pin', () => {
  it('was generated from the PerpsVault.sol of this tree (re-pin and re-verify when the contract changes)', () => {
    // jsdom gives import.meta.url an https scheme: resolve from the frontend directory (vitest's root).
    const source = readFileSync(resolve(process.cwd(), '../contracts/src/PerpsVault.sol'));
    expect(createHash('sha256').update(source).digest('hex')).toBe(VAULT_SOURCE_SHA256);
  });
  it('holds every function the page calls with the documented signature (docs/VAULT.md 4)', () => {
    const signatures = new Set(
      (VAULT_ABI as unknown as { type: string; name?: string; inputs?: { type: string; components?: { type: string }[] }[] }[])
        .filter((e) => e.type === 'function')
        .map((e) => `${e.name}(${e.inputs!.map((i) => (i.components ? `(${i.components.map((c) => c.type).join(',')})${i.type.slice(5)}` : i.type)).join(',')})`),
    );
    for (const s of [
      'deposit(uint64,address)',
      'requestForced(uint8,uint64)',
      'withdraw(uint64,(uint64,address,address,uint64,uint64),bytes32[],uint64)',
      'release(bytes32,uint64)',
      'triggerEscape()',
      'claimEscape((address,uint32,int128,uint64,bytes32),bytes32[],uint64)',
      'proveSystemShares((address,uint32,int128,uint64,bytes32),bytes32[],(address,uint32,int128,uint64,bytes32),bytes32[])',
      'refundDeposit(uint64,address,uint64)',
    ])
      expect(signatures.has(s), s).toBe(true);
  });
  it('every custom error of the vault has words (an unexplained revert is a defect of this interface)', () => {
    const errors = (VAULT_ABI as readonly { type: string; name?: string }[]).filter((e) => e.type === 'error').map((e) => e.name!);
    // Errors only a relayer, a guardian or the deployer can meet are named by their identifier.
    const notUserFacing = ['InvalidConfiguration', 'InvalidCheckpoint', 'InvalidCertificate', 'NotGuardian', 'InvalidPause', 'InvalidSurplus'];
    for (const e of errors) if (!notUserFacing.includes(e)) expect(REVERT_WORDS[e], e).toBeTruthy();
  });
});

describe('deployment and amounts', () => {
  it('no vault, no token, the zero address or vault == token: no deployment (nothing can be built)', () => {
    expect(deploymentOf({ l2ChainId: 700770, vault: null, collateral: TOKEN })).toBeNull();
    expect(deploymentOf({ l2ChainId: 700770, vault: VAULT, collateral: null })).toBeNull();
    expect(deploymentOf({ l2ChainId: 700770, vault: `0x${'00'.repeat(20)}`, collateral: TOKEN })).toBeNull();
    expect(deploymentOf({ l2ChainId: 700770, vault: VAULT, collateral: VAULT })).toBeNull();
    expect(deploymentOf({ l2ChainId: 700770, vault: VAULT.toUpperCase().replace('0X', '0x'), collateral: TOKEN })).toEqual(d);
  });
  it('amounts are written exactly, to six places when they have them', () => {
    expect(usdc(1_234_567n)).toBe('1.234567 bUSDC');
    expect(usdc(1_000_000n)).toBe('1.00 bUSDC');
    expect(usdc(1_500_000_000n)).toBe('1,500.00 bUSDC');
    expect(usdc(1n)).toBe('0.000001 bUSDC');
  });
});

describe('states in words', () => {
  it('phase', () => {
    expect(phaseOf(view())).toBe('normal');
    expect(phaseOf(view({ paused: true }))).toBe('paused');
    expect(phaseOf(view({ escapeReason: 2, paused: true }))).toBe('escapeDue');
    expect(phaseOf(view({ escapeAt: 5n }))).toBe('escapeUnproven');
    expect(phaseOf(view({ escapeAt: 5n, systemSharesProven: true }))).toBe('escape');
  });
  it('silence countdown: none before the first checkpoint, zero when due', () => {
    expect(silenceLeft(view({ lastCheckpointAt: 0n }))).toBeNull();
    expect(silenceLeft(view())).toBe(604_800n - 1_000n);
    expect(silenceLeft(view({ now: 1_900_000_000n }))).toBe(0n);
  });
  it('payout states', () => {
    expect(payoutState({ recipient: USER, total: 0n, paid: 0n, readyAt: 0n }, 10n)).toBe('none');
    expect(payoutState({ recipient: USER, total: 5n, paid: 0n, readyAt: 11n }, 10n)).toBe('waiting');
    expect(payoutState({ recipient: USER, total: 5n, paid: 0n, readyAt: 10n }, 10n)).toBe('payable');
    expect(payoutState({ recipient: USER, total: 5n, paid: 2n, readyAt: 10n }, 10n)).toBe('partly');
    expect(payoutState({ recipient: USER, total: 5n, paid: 5n, readyAt: 99n }, 10n)).toBe('paid');
  });
  it('deposit refusals', () => {
    expect(depositProblems(view(), 100n * U)).toEqual([]);
    expect(depositProblems(view(), null)).toEqual(['Enter an amount.']);
    expect(depositProblems(view(), 10n * U - 1n).join(' ')).toMatch(/minimum deposit is 10\.00 bUSDC/);
    expect(depositProblems(view({ custody: 999_950n * U }), 100n * U).join(' ')).toMatch(/at most 50\.00 bUSDC/);
    expect(depositProblems(view({}, { balance: 99n * U }), 100n * U).join(' ')).toMatch(/holds 99\.00 bUSDC/);
    expect(depositProblems(view({ paused: true }), 100n * U).join(' ')).toMatch(/paused/);
    expect(depositProblems(view({ escapeAt: 1n }), 100n * U).join(' ')).toMatch(/escape mode/);
    expect(depositProblems(view({}, null), 100n * U).join(' ')).toMatch(/Connect a wallet/);
    expect(depositProblems(view(), 1n << 64n).join(' ')).toMatch(/too large/);
  });
  it('forced-request refusals', () => {
    expect(forcedProblems(view(), 'withdraw', 0n)).toEqual([]);
    expect(forcedProblems(view({}, { hasDeposited: false }), 'cancelAll', 0n).join(' ')).toMatch(/deposited through the vault/);
    expect(forcedProblems(view({}, { forcedOpen: true }), 'cancelAll', 0n).join(' ')).toMatch(/already has a request open/);
    expect(forcedProblems(view(), 'closeAll', 5n).join(' ')).toMatch(/Only a withdrawal request carries an amount/);
    expect(forcedProblems(view({ escapeAt: 1n }), 'withdraw', 0n).join(' ')).toMatch(/escape mode/);
  });
  it('escape estimate = the vault\'s formula, rounded down, null until the ratio is fixed', () => {
    expect(escapeAmount(view(), 100n)).toBeNull();
    expect(escapeAmount(view({ escapeAt: 1n }), 100n)).toBeNull();
    const short = view({ escapeAt: 1n, systemSharesProven: true, escapePayable: 2_000n * U, subordinatedTotal: 300n * U, latest: { ...view().latest, totalEscape: 2_500n * U } });
    // senior = 2,200; payable 2,000: 1,500 * 2000 / 2200 = 1,363.636363 (floor), as VAULT.md's R395-33 example.
    expect(escapeAmount(short, 1_500n * U)).toBe(1_363_636_363n);
    const full = view({ escapeAt: 1n, systemSharesProven: true, escapePayable: 1_900n * U, subordinatedTotal: 300n * U, latest: { ...view().latest, totalEscape: 2_200n * U } });
    expect(escapeAmount(full, 1_500n * U)).toBe(1_500n * U);
    expect(escapeAmount(view({ escapeAt: 1n, systemSharesProven: true, subordinatedTotal: 1_000n * U }), 5n)).toBeNull();
  });
});

describe('transactions: a closed list, described from the calldata', () => {
  it('approve is for the exact amount and only to the vault', () => {
    const tx = buildTx(d, { kind: 'approve', amount: 250n * U });
    expect(tx.to).toBe(TOKEN);
    const words = describeTx(d, tx);
    expect(words.title).toBe('Allow the vault to take 250.00 bUSDC from this wallet');
    expect(words.notes.join(' ')).toMatch(/never unlimited/);
    const toOther = encodeFunctionData({ abi: TOKEN_ABI, functionName: 'approve', args: [OTHER, 1n] });
    expect(() => describeTx(d, { to: TOKEN, data: toOther })).toThrow(UnknownTx);
    const unlimited = encodeFunctionData({ abi: TOKEN_ABI, functionName: 'approve', args: [VAULT, (1n << 256n) - 1n] });
    expect(() => describeTx(d, { to: TOKEN, data: unlimited })).toThrow(/unlimited/);
    expect(() => buildTx(d, { kind: 'approve', amount: 1n << 64n })).toThrow(RangeError);
  });
  it('the description comes from the bytes: a deposit built for another account says that account', () => {
    const words = describeTx(d, buildTx(d, { kind: 'deposit', amount: 1_234_567n, account: OTHER }));
    expect(words.title).toBe('Deposit 1.234567 bUSDC into the exchange vault');
    expect(words.rows.find((r) => r.label === 'Credited to trading account')?.value).toBe(OTHER);
    expect(() => buildTx(d, { kind: 'deposit', amount: 0n, account: USER })).toThrow(RangeError);
  });
  it('forced requests', () => {
    expect(describeTx(d, buildTx(d, { kind: 'requestForced', forced: 'withdraw', arg: 0n })).rows.find((r) => r.label === 'Amount')?.value).toBe('All free collateral');
    expect(describeTx(d, buildTx(d, { kind: 'requestForced', forced: 'withdraw', arg: 5n * U })).rows.find((r) => r.label === 'Amount')?.value).toBe('5.00 bUSDC');
    expect(describeTx(d, buildTx(d, { kind: 'requestForced', forced: 'closeAll', arg: 0n })).rows[0].value).toBe('Close all my positions');
    expect(() => buildTx(d, { kind: 'requestForced', forced: 'cancelAll', arg: 1n })).toThrow(/only a withdrawal/);
    const badKind = encodeFunctionData({ abi: VAULT_ABI, functionName: 'requestForced', args: [7, 0n] } as never);
    expect(() => describeTx(d, { to: VAULT, data: badKind })).toThrow(UnknownTx);
  });
  it('a withdrawal claim names the recipient and amount of the RECORD', () => {
    const record = { withdrawalId: 4n, account: USER, recipient: OTHER, amount: 99n * U, height: 7n };
    const words = describeTx(d, buildTx(d, { kind: 'withdraw', seq: 2n, record, siblings: Array<Hex>(32).fill(ZERO32), want: 99n * U }));
    expect(words.title).toBe(`Collect withdrawal #4: 99.00 bUSDC to ${OTHER}`);
    expect(words.rows.find((r) => r.label === 'Paid to')?.value).toBe(OTHER);
  });
  it('escape transactions say what they are; triggerEscape says it is one way', () => {
    expect(describeTx(d, buildTx(d, { kind: 'triggerEscape' })).notes.join(' ')).toMatch(/ONE WAY/);
    const leaf = { account: USER, index: 3, balance: 5n, escapeValue: 750n * U, positionsHash: ZERO32 };
    expect(describeTx(d, buildTx(d, { kind: 'claimEscape', leaf, siblings: Array<Hex>(32).fill(ZERO32), want: 1n })).title).toBe(`Claim the escape balance of ${USER}`);
    expect(describeTx(d, buildTx(d, { kind: 'refundDeposit', index: 2n, account: USER, amount: 100n * U })).title).toBe(`Refund deposit #2: 100.00 bUSDC to ${USER}`);
  });
  it('anything else is not described: another contract, a vault function outside the list, junk', () => {
    expect(() => describeTx(d, { to: OTHER, data: '0x' })).toThrow(UnknownTx);
    expect(() => describeTx(d, { to: VAULT, data: '0xdeadbeef' })).toThrow(UnknownTx);
    expect(() => describeTx(d, { to: VAULT, data: encodeFunctionData({ abi: VAULT_ABI, functionName: 'guardianPause' } as never) })).toThrow(/never calls/);
    expect(() => describeTx(d, { to: VAULT, data: encodeFunctionData({ abi: VAULT_ABI, functionName: 'sweepEscapeSurplus' } as never) })).toThrow(/never calls/);
    expect(() => describeTx(d, { to: TOKEN, data: '0xa9059cbb' })).toThrow(UnknownTx);
  });
  it('F434-1 gas limit: 1.5x + 40,000, capped at the L2 limit; an estimate above the cap or of zero is refused', () => {
    expect(gasLimit(168_094n)).toBe(292_141n);
    expect(gasLimit(L2_TX_GAS_CAP)).toBe(L2_TX_GAS_CAP);
    expect(() => gasLimit(L2_TX_GAS_CAP + 1n)).toThrow(TxRefused);
    expect(() => gasLimit(0n)).toThrow(TxRefused);
  });
  it('reverts in words; an unknown revert is said to be unknown', () => {
    expect(revertWords(encodeErrorResult({ abi: VAULT_ABI, errorName: 'Paused' } as never))).toMatch(/paused by a guardian/);
    expect(revertWords(encodeErrorResult({ abi: VAULT_ABI, errorName: 'EscapeNotDue' } as never))).toMatch(/escape is not due/);
    expect(revertWords('0x12345678')).toMatch(/would be rejected/);
    expect(revertWords(null)).toMatch(/would reject/);
  });
});

/** A scripted JSON-RPC endpoint. `on(method, params)` returns the result or throws { data } for a revert. */
function endpoint(on: (method: string, params: unknown[]) => unknown, chain = '0xab162') {
  const seen: string[] = [];
  const answer = (req: { id: number; method: string; params: unknown[] }) => {
    seen.push(req.method);
    if (req.method === 'eth_chainId') return { jsonrpc: '2.0', id: req.id, result: chain };
    try {
      return { jsonrpc: '2.0', id: req.id, result: on(req.method, req.params) };
    } catch (e) {
      return { jsonrpc: '2.0', id: req.id, error: { code: 3, message: 'execution reverted', data: (e as { data?: string }).data } };
    }
  };
  const fetchImpl: Fetch = async (_url, init) => {
    const body = JSON.parse(String(init.body));
    return new Response(JSON.stringify(Array.isArray(body) ? body.map(answer).reverse() : answer(body)), { status: 200 });
  };
  return { l2: new L2('http://l2.invalid', 700770, fetchImpl), seen };
}

describe('L2 client', () => {
  it('refuses an endpoint on another chain before any read, and checks again after a failure', async () => {
    const { l2, seen } = endpoint(() => '0x', '0x1');
    await expect(l2.call({ to: VAULT, data: '0x' })).rejects.toThrow(/is chain 1, not the Bloch L2 \(chain 700770\)/);
    await expect(l2.head()).rejects.toThrow(L2Error);
    expect(seen.filter((m) => m !== 'eth_chainId')).toEqual([]);
  });
  it('matches batch answers by id whatever their order', async () => {
    const { l2 } = endpoint((_m, p) => (p[0] as { data: string }).data);
    expect(await l2.calls([{ to: VAULT, data: '0x01' }, { to: VAULT, data: '0x02' }, { to: VAULT, data: '0x03' }], 5n)).toEqual(['0x01', '0x02', '0x03']);
  });
  it('carries revert data out of eth_call', async () => {
    const data = encodeErrorResult({ abi: VAULT_ABI, errorName: 'RateLimited' } as never);
    const { l2 } = endpoint(() => {
      throw { data };
    });
    await expect(l2.call({ to: VAULT, data: '0x' })).rejects.toMatchObject({ revertData: data });
  });
  it('refuses unusable answers: an odd-length result, the receipt of another transaction, a status that is not 0 or 1', async () => {
    await expect(endpoint(() => '0x123').l2.call({ to: VAULT, data: '0x' })).rejects.toThrow(/unusable call result/);
    const hash = `0x${'11'.repeat(32)}` as Hex;
    const receipt = (extra: object) => ({ transactionHash: hash, status: '0x1', blockNumber: '0x5', logs: [], ...extra });
    await expect(endpoint(() => receipt({ transactionHash: `0x${'22'.repeat(32)}` })).l2.receipt(hash)).rejects.toThrow(/another transaction/);
    await expect(endpoint(() => receipt({ status: '0x2' })).l2.receipt(hash)).rejects.toThrow(/receipt status/);
    expect(await endpoint(() => null).l2.receipt(hash)).toBeNull();
    expect((await endpoint(() => receipt({ status: '0x0' })).l2.receipt(hash))?.status).toBe('reverted');
  });
});

describe('sendTx: the wallet is asked last, and only for what was simulated', () => {
  const tx = buildTx(d, { kind: 'deposit', amount: 100n * U, account: USER });
  function wallet(chain = '0xab162', account: string = USER) {
    const asked: { method: string; params?: unknown[] }[] = [];
    const request: WalletRequest = async (a) => {
      asked.push(a);
      if (a.method === 'eth_chainId') return chain;
      if (a.method === 'eth_accounts') return [account];
      if (a.method === 'eth_sendTransaction') return `0x${'ab'.repeat(32)}`;
      throw new Error('unsupported');
    };
    return { request, asked, sent: () => asked.filter((a) => a.method === 'eth_sendTransaction') };
  }
  const ok = () => endpoint((m) => (m === 'eth_estimateGas' ? '0x30d40' : encodeFunctionResult({ abi: VAULT_ABI, functionName: 'deposit', result: 0n } as never)));

  it('sends exactly the built bytes, to the pinned contract, with no value and the page\'s gas limit', async () => {
    const w = wallet();
    const hash = await sendTx(w.request, ok().l2, d, USER.toUpperCase().replace('0X', '0x'), tx);
    expect(hash).toBe(`0x${'ab'.repeat(32)}`);
    expect(w.sent()).toEqual([{ method: 'eth_sendTransaction', params: [{ from: USER, to: VAULT, data: tx.data, value: '0x0', gas: `0x${gasLimit(200_000n).toString(16)}` }] }]);
  });
  it('a simulated revert stops before the wallet, in the vault\'s words', async () => {
    const w = wallet();
    const reverting = endpoint(() => {
      throw { data: encodeErrorResult({ abi: VAULT_ABI, errorName: 'Paused' } as never) };
    });
    await expect(sendTx(w.request, reverting.l2, d, USER, tx)).rejects.toThrow(/paused by a guardian/);
    expect(w.sent()).toEqual([]);
  });
  it('an unreachable L2, a wrong wallet chain, another wallet account, an undescribable transaction: nothing sent', async () => {
    const dead = new L2('http://l2.invalid', 700770, async () => {
      throw new Error('down');
    });
    for (const [w, l2, t] of [
      [wallet(), dead, tx],
      [wallet('0x1'), ok().l2, tx],
      [wallet('0xab162', OTHER), ok().l2, tx],
      [wallet(), ok().l2, { ...tx, to: OTHER }],
      [wallet(), ok().l2, { ...tx, data: '0xdeadbeef' as Hex }],
    ] as const) {
      await expect(sendTx(w.request, l2, d, USER, t)).rejects.toThrow(TxRefused);
      expect(w.sent()).toEqual([]);
    }
  });
  it('a wallet that returns no hash is reported, never treated as sent', async () => {
    const w = wallet();
    const request: WalletRequest = async (a) => (a.method === 'eth_sendTransaction' ? null : w.request(a));
    await expect(sendTx(request, ok().l2, d, USER, tx)).rejects.toThrow(/returned no transaction hash/);
  });
});
