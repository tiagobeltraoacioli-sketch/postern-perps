/** docs/VAULT.md 5.4 / 5.5 formats against the SHARED vectors of contracts/ (agent 390/433), not against this code. */
import { describe, expect, it } from 'vitest';
import { keccak256, stringToHex, type Hex } from 'viem';
import vectors from '../../../contracts/test/vectors/vault-vectors.json';
import { accountLeaf, depositHead0, forcedHead0, rootFromProof, SparseTree, withdrawalLeaf, ZERO, TREE_DEPTH } from '../vault/merkle';

const k = (text: string) => keccak256(stringToHex(text));
const out = vectors.outputs;

describe('vault Merkle formats (vault-vectors 0.3)', () => {
  it('withdrawal leaf', () => {
    const w = vectors.inputs.withdrawal;
    expect(withdrawalLeaf({ withdrawalId: BigInt(w.withdrawalId), account: w.account as Hex, recipient: w.recipient as Hex, amount: BigInt(w.amount), height: BigInt(w.height) })).toBe(out.withdrawalLeaf);
  });
  it('account leaf with a negative balance (two\'s complement, 128 bits)', () => {
    const a = vectors.inputs.account;
    expect(accountLeaf({ account: a.account as Hex, index: a.index, balance: BigInt(a.balance), escapeValue: BigInt(a.escapeValue), positionsHash: k('positions') })).toBe(out.accountLeaf);
  });
  it('empty root and the three-leaf root', () => {
    expect(ZERO[TREE_DEPTH]).toBe(out.root32_empty);
    expect(new SparseTree([]).root).toBe(out.root32_empty);
    expect(new SparseTree([k('a'), k('b'), k('c')]).root).toBe(out.root32_abc);
  });
  it('every proof of the three-leaf tree reaches its root; another position, leaf or sibling does not', () => {
    const leaves = [k('a'), k('b'), k('c')];
    const tree = new SparseTree(leaves);
    for (let i = 0; i < 3; i++) {
      const proof = tree.proof(i);
      expect(rootFromProof(leaves[i], BigInt(i), proof)).toBe(out.root32_abc);
      expect(rootFromProof(leaves[i], BigInt(i ^ 1), proof)).not.toBe(out.root32_abc);
      expect(rootFromProof(k('x'), BigInt(i), proof)).not.toBe(out.root32_abc);
      const bent = [...proof];
      bent[7] = k('bent');
      expect(rootFromProof(leaves[i], BigInt(i), bent)).not.toBe(out.root32_abc);
    }
  });
  it('a proof that is not exactly 32 hashes, or a position outside the tree, is refused (never padded)', () => {
    const tree = new SparseTree([k('a')]);
    expect(() => rootFromProof(k('a'), 0n, tree.proof(0).slice(1))).toThrow(/exactly 32/);
    expect(() => rootFromProof(k('a'), 0n, [...tree.proof(0), ZERO[0]])).toThrow(/exactly 32/);
    expect(() => rootFromProof(k('a'), 1n << 32n, tree.proof(0))).toThrow(/outside/);
    expect(() => rootFromProof(k('a'), 0n, ['0x12', ...tree.proof(0).slice(1)])).toThrow(/32 bytes/);
  });
  it('chain heads', () => {
    expect(depositHead0(vectors.chainId, vectors.vault as Hex)).toBe(out.depositHead0);
    expect(forcedHead0(vectors.chainId, vectors.vault as Hex)).toBe(out.forcedHead0);
  });
  it('out-of-range leaf fields are refused, not wrapped', () => {
    const w = { withdrawalId: 1n, account: `0x${'11'.repeat(20)}` as Hex, recipient: `0x${'22'.repeat(20)}` as Hex, amount: 1n, height: 1n };
    expect(() => withdrawalLeaf({ ...w, amount: 1n << 64n })).toThrow(/amount/);
    expect(() => withdrawalLeaf({ ...w, withdrawalId: -1n })).toThrow(/withdrawalId/);
    expect(() => withdrawalLeaf({ ...w, recipient: '0x1234' as Hex })).toThrow(/recipient/);
    expect(() => accountLeaf({ account: w.account, index: 2 ** 32, balance: 0n, escapeValue: 0n, positionsHash: k('p') })).toThrow(/index/);
    expect(() => accountLeaf({ account: w.account, index: 0, balance: 1n << 127n, escapeValue: 0n, positionsHash: k('p') })).toThrow(/balance/);
  });
});
