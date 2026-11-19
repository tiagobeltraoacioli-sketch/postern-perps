/**
 * A local PerpsVault on anvil with a stand-in for what the engine and the committee supply: account and withdrawal
 * trees (src/vault/merkle.ts, checked against the engine's own vectors), and checkpoints signed by 5 of 7 throwaway
 * keys generated in this process and never written. Test and rehearsal use only. UNAUDITED prototype.
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { decodeFunctionResult, encodeDeployData, encodeFunctionData, type Abi, type Hex } from 'viem';
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';
import { TOKEN_ABI, VAULT_ABI } from '../../src/vault/abi';
import { accountLeaf, SparseTree, withdrawalLeaf, type AccountLeaf, type WithdrawalRecord } from '../../src/vault/merkle';

export const CHAIN = 700770;
export const U = 1_000_000n; // 1 bUSDC
export const OWNER = '0x775a993f09bd80aecf8f8c2ade0c84158e3a0544' as Hex;
export const ZERO32 = `0x${'00'.repeat(32)}` as Hex;
export const keyless = (n: number) => `0x${n.toString(16).padStart(40, '0')}` as Hex;

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

export type StandIn = Awaited<ReturnType<typeof standIn>>;

/** Deploys the mock token and the vault on the anvil at `rpcUrl` from the forge build in `forgeOut`. */
export async function standIn(rpcUrl: string, forgeOut: string) {
  let id = 0;
  const rpc = async (method: string, params: unknown[] = []): Promise<unknown> => {
    const r = await fetch(rpcUrl, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: ++id, method, params }) });
    const j = (await r.json()) as { result?: unknown; error?: { message: string } };
    if (j.error) throw new Error(`${method}: ${j.error.message}`);
    return j.result;
  };
  const artifact = (path: string) => JSON.parse(readFileSync(resolve(forgeOut, path), 'utf8')) as { abi: Abi; bytecode: { object: Hex } };
  const mined = async (hash: Hex) => {
    // The receipt can lag the answer of eth_sendTransaction by a moment even with automine.
    for (let i = 0; i < 200; i++) {
      const r = (await rpc('eth_getTransactionReceipt', [hash])) as { status: string; contractAddress: Hex | null } | null;
      if (r) {
        if (r.status !== '0x1') throw new Error('setup transaction reverted');
        return r;
      }
      await new Promise((done) => setTimeout(done, 25));
    }
    throw new Error('setup transaction not mined');
  };
  const send = async (from: Hex, to: Hex, data: Hex) => mined((await rpc('eth_sendTransaction', [{ from, to, data, gas: '0x2dc6c0' }])) as Hex);
  const deploy = async (from: Hex, a: { abi: Abi; bytecode: { object: Hex } }, args: unknown[]) =>
    ((await mined((await rpc('eth_sendTransaction', [{ from, data: encodeDeployData({ abi: a.abi, bytecode: a.bytecode.object, args }) }])) as Hex)).contractAddress as Hex).toLowerCase() as Hex;

  const accounts = ((await rpc('eth_accounts')) as Hex[]).map((a) => a.toLowerCase() as Hex);
  const committee = Array.from({ length: 7 }, () => privateKeyToAccount(generatePrivateKey())).sort((a, b) => (a.address.toLowerCase() < b.address.toLowerCase() ? -1 : 1));
  const tokenArtifact = artifact('Base.sol/MockUSDC.json');
  const token = await deploy(accounts[0], tokenArtifact, []);
  const config = {
    token,
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
    guardians: accounts.slice(6, 9).sort(),
    pauseDuration: 2n * 86_400n,
    pauseCooldown: 2n * 86_400n,
  };
  const vault = await deploy(accounts[0], artifact('PerpsVault.sol/PerpsVault.json'), [config]);
  const view = async (name: string, args: unknown[] = []) =>
    decodeFunctionResult({ abi: VAULT_ABI, functionName: name, data: (await rpc('eth_call', [{ to: vault, data: encodeFunctionData({ abi: VAULT_ABI, functionName: name, args } as never) }, 'latest'])) as Hex } as never) as unknown;

  // The engine stand-in's state: account leaves (index = position) and withdrawal records (id = position).
  const state = { leaves: [] as AccountLeaf[], records: [] as WithdrawalRecord[], height: 0n };
  const accountTree = () => new SparseTree(state.leaves.map(accountLeaf));
  const withdrawalTree = () => new SparseTree(state.records.map(withdrawalLeaf));

  return {
    rpc,
    accounts,
    committee: committee.map((v) => v.address),
    deployment: { chainId: CHAIN, vault, token },
    vaultArtifactAbi: artifact('PerpsVault.sol/PerpsVault.json').abi,
    state,
    accountTree,
    withdrawalTree,
    view,
    send,
    mint: (to: Hex, amount: bigint) => send(accounts[0], token, encodeFunctionData({ abi: tokenArtifact.abi, functionName: 'mint', args: [to, amount] })),
    balanceOf: async (who: Hex) => decodeFunctionResult({ abi: TOKEN_ABI, functionName: 'balanceOf', data: (await rpc('eth_call', [{ to: token, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'balanceOf', args: [who] }) }, 'latest'])) as Hex }) as bigint,
    /** Moves the L2 clock forward and mines a block. */
    warp: async (seconds: bigint) => {
      await rpc('evm_increaseTime', [Number(seconds)]);
      await rpc('evm_mine');
    },
    /** Signs (5 of 7) and submits a checkpoint over the current stand-in state, crediting deposits and forced requests up to the given counts. */
    checkpoint: async (nextDepositIndex: bigint, nextForcedIndex: bigint, relayer: Hex) => {
      state.height += 10n;
      const cp = {
        height: state.height,
        time: 1_800_000_000_000n + state.height,
        logHash: ZERO32,
        accountsRoot: accountTree().root,
        marketsRoot: ZERO32,
        withdrawalsRoot: withdrawalTree().root,
        nextDepositIndex,
        nextForcedIndex,
        nextOrderId: 1n,
        totalEscape: state.leaves.reduce((s, l) => s + l.escapeValue, 0n),
        dust: 0n,
        auxHash: ZERO32,
        depositsHead: (await view('depositHead', [nextDepositIndex])) as Hex,
        forcedHead: (await view('forcedHead', [nextForcedIndex])) as Hex,
        withdrawalCount: BigInt(state.records.length),
        withdrawalsTotal: state.records.reduce((s, r) => s + r.amount, 0n),
      };
      const domain = { name: 'PosternPerpsVault', version: '1', chainId: CHAIN, verifyingContract: vault };
      const signatures = await Promise.all(committee.slice(0, 5).map((v) => v.signTypedData({ domain, types: CHECKPOINT_TYPES, primaryType: 'Checkpoint', message: cp })));
      await send(relayer, vault, encodeFunctionData({ abi: VAULT_ABI, functionName: 'submitCheckpoint', args: [cp, signatures] } as never));
      return cp;
    },
  };
}
