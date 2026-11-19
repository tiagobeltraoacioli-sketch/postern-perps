// SPEC 11 / VAULT.md 5.5 trees against the vault's own vectors (contracts/test/vectors/vault-vectors.json, pinned by
// the Solidity tests): the JS here was written from the documents, so agreement is an independent cross-check.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { hex, unhex } from '../lib/keccak.mjs';
import { ZERO, leafHash, tree32, root32, encodeAccountLeaf, decodeAccountLeaf, encodeWithdrawalLeaf, positionsHash, DEPTH } from '../lib/merkle32.mjs';

const V = JSON.parse(readFileSync(new URL('../../../contracts/test/vectors/vault-vectors.json', import.meta.url), 'utf8'));
const find = (o, pred, path = '') => {
  const out = [];
  if (o && typeof o === 'object') for (const [k, v] of Object.entries(o)) { if (pred(k, v)) out.push([`${path}/${k}`, v]); out.push(...find(v, pred, `${path}/${k}`)); }
  return out;
};

test('the empty depth-32 root equals the vault vector', () => {
  const empties = find(V, (k, v) => typeof v === 'string' && /empty/i.test(k) && /^0x[0-9a-f]{64}$/.test(v));
  assert.ok(empties.length >= 1, 'the vector file names an empty root');
  assert.ok(empties.some(([, v]) => v === hex(ZERO[DEPTH])), `Z(32) ${hex(ZERO[DEPTH])} not among ${JSON.stringify(empties)}`);
  assert.equal(tree32(new Map()).root, hex(ZERO[DEPTH]));
});
test('proofs: every leaf of a sparse tree walks to the root; a wrong leaf, index or sibling does not; exactly 32 siblings', () => {
  const leaves = new Map([[0, leafHash(Uint8Array.of(1))], [1, leafHash(Uint8Array.of(2))], [2, leafHash(Uint8Array.of(3))], [7, leafHash(Uint8Array.of(4))], [4294967295, leafHash(Uint8Array.of(5))]]);
  const t = tree32(leaves);
  for (const [i, l] of leaves) {
    const p = t.proof(i);
    assert.equal(p.length, 32);
    assert.equal(root32(l, i, p), t.root);
    assert.notEqual(root32(leafHash(Uint8Array.of(9)), i, p), t.root);
    assert.notEqual(root32(l, i % 2 === 0 ? i + 1 : i - 1, p), t.root);
    assert.notEqual(root32(l, i, [p[1], p[0], ...p.slice(2)]), t.root);
    assert.throws(() => root32(l, i, p.slice(1)));
  }
  // an absent position proves the zero leaf, never a value
  assert.equal(root32(ZERO[0], 3, t.proof(3)), t.root);
});
test('account leaf: 88 bytes, round trip, signed balance, and the documented field order', () => {
  const a = { account: '0x' + 'ab'.repeat(20), index: 12, balance: '-5', escapeValue: '0', positionsHash: hex(positionsHash([])) };
  const e = encodeAccountLeaf(a);
  assert.equal(e.length, 88);
  assert.deepEqual(decodeAccountLeaf(e), a);
  assert.equal(hex(e.slice(32, 48)), '0x' + 'ff'.repeat(15) + 'fb');
  assert.equal(new TextDecoder().decode(e.slice(0, 8)), 'PPv1acct');
  assert.throws(() => decodeAccountLeaf(e.slice(1)));
  assert.equal(encodeWithdrawalLeaf({ withdrawalId: 1, account: a.account, recipient: a.account, amount: 5, height: 9 }).length, 72);
  // positions are hashed in ascending market id whatever the input order
  const p1 = { marketId: 1, size: 5, openNotional: -7n, isoMargin: 0, isolated: false }, p2 = { marketId: 2, size: -5, openNotional: 7n, isoMargin: 0, isolated: false };
  assert.equal(hex(positionsHash([p1, p2])), hex(positionsHash([p2, p1])));
  assert.notEqual(hex(positionsHash([p1])), hex(positionsHash([p2])));
});
test('the vault vectors: withdrawal leaf, account leaf (negative balance), root of three leaves', async () => {
  const { keccak256 } = await import('../lib/keccak.mjs');
  const k = (s) => keccak256(new TextEncoder().encode(s));
  const w = V.inputs.withdrawal;
  assert.equal(hex(leafHash(encodeWithdrawalLeaf({ ...w, account: w.account.toLowerCase(), recipient: w.recipient.toLowerCase() }))), V.outputs.withdrawalLeaf);
  const a = V.inputs.account;
  assert.equal(hex(leafHash(encodeAccountLeaf({ account: a.account.toLowerCase(), index: a.index, balance: a.balance, escapeValue: a.escapeValue, positionsHash: k('positions') }))), V.outputs.accountLeaf);
  assert.equal(tree32(new Map([[0, k('a')], [1, k('b')], [2, k('c')]])).root, V.outputs.root32_abc);
  assert.equal(hex(ZERO[DEPTH]), V.outputs.root32_empty);
});
