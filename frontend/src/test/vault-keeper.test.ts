/** R436-11 (c): the user's own exit proof is kept on the device, only after it was checked against the vault. */
import { describe, expect, it } from 'vitest';
import type { Hex } from 'viem';
import { accountLeaf, SparseTree, type AccountLeaf } from '../vault/merkle';
import { keepProof, keptProofFor, keptProofs, KEPT, proofFile } from '../vault/keeper';
import { parsePastedProof } from '../vault/wire';
import { checkEscapeClaim, type VaultView } from '../vault/vault';

const VAULT = `0x${'aa'.repeat(20)}` as Hex;
const USER = `0x${'c1'.repeat(20)}` as Hex;
const OTHER = `0x${'d2'.repeat(20)}` as Hex;
const Z = `0x${'00'.repeat(32)}` as Hex;
const leafOf = (account: Hex, index: number, value: bigint): AccountLeaf => ({ account, index, balance: value, escapeValue: value, positionsHash: Z });

function checkpoint(value: bigint) {
  const leaves = [leafOf(`0x${'0'.repeat(39)}1` as Hex, 0, 0n), leafOf(`0x${'0'.repeat(39)}2` as Hex, 1, 5n), leafOf(`0x${'0'.repeat(39)}3` as Hex, 2, 5n), leafOf(USER, 3, value), leafOf(OTHER, 4, 9n)];
  const tree = new SparseTree(leaves.map(accountLeaf));
  return { leaf: leaves[3], siblings: tree.proof(3), accountsRoot: tree.root };
}
const entry = (height: bigint, value: bigint) => ({ chainId: 700770, vault: VAULT, account: USER, height, ...checkpoint(value) });

describe('exit proof keeper', () => {
  it('keeps a proof and returns it for exactly the root it was checked against', () => {
    const e = entry(100n, 750_000_000n);
    expect(keepProof(e, 1_000)).toBe(true);
    const hit = keptProofFor(VAULT, USER, e.accountsRoot)!;
    expect(hit.leaf).toEqual(e.leaf);
    expect(hit.siblings).toEqual(e.siblings);
    expect(keptProofFor(VAULT, USER, Z)).toBeNull();
    expect(keptProofFor(VAULT, OTHER, e.accountsRoot)).toBeNull();
    expect(keptProofFor(OTHER, USER, e.accountsRoot)).toBeNull();
  });
  it('the kept proof passes the same check the claim uses against the vault view, and fails for a later root', () => {
    const e = entry(100n, 750_000_000n);
    keepProof(e);
    const hit = keptProofFor(VAULT, USER, e.accountsRoot)!;
    const view = { latest: { accountsRoot: e.accountsRoot } } as VaultView;
    expect(() => checkEscapeClaim(view, hit.leaf, hit.siblings, USER)).not.toThrow();
    const later = { latest: { accountsRoot: checkpoint(1n).accountsRoot } } as VaultView;
    expect(() => checkEscapeClaim(later, hit.leaf, hit.siblings, USER)).toThrow(/does not match the last checkpoint/);
  });
  it(`keeps the newest ${KEPT} checkpoints, newest first; the same root is stored once`, () => {
    for (let h = 1n; h <= 7n; h++) keepProof(entry(h * 10n, h), Number(h));
    keepProof(entry(70n, 7n), 99);
    const list = keptProofs(VAULT, USER);
    expect(list.map((p) => p.height)).toEqual(['70', '60', '50', '40']);
    expect(list[0].savedAt).toBe(99);
  });
  it('a proof whose leaf is another account is not stored; corrupted storage reads as nothing', () => {
    const e = entry(100n, 5n);
    expect(keepProof({ ...e, account: OTHER })).toBe(false);
    localStorage.setItem(`postern.perps.exitproof.v1:${VAULT}:${USER}`, '{"not":"a list"}');
    expect(keptProofs(VAULT, USER)).toEqual([]);
    localStorage.setItem(`postern.perps.exitproof.v1:${VAULT}:${USER}`, JSON.stringify([{ height: '1', accountsRoot: Z, savedAt: 1, leafData: {}, proof: [] }]));
    expect(keptProofs(VAULT, USER)).toEqual([]);
  });
  it('the file can be pasted back on the Exit tab and yields the same leaf and siblings', () => {
    const e = entry(100n, 750_000_000n);
    keepProof(e, Date.UTC(2026, 10, 18));
    const file = proofFile(keptProofs(VAULT, USER)[0]);
    expect(file.name).toBe('postern-perps-exit-proof-0xc1c1c1c1-h100.json');
    expect(file.text).toMatch(/not a key/);
    const back = parsePastedProof(file.text);
    expect(back.leaf).toEqual(e.leaf);
    expect(back.siblings).toEqual(e.siblings);
  });
});
