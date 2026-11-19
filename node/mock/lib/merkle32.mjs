// docs/SPEC.md section 11 / docs/VAULT.md 5.5 trees: depth 32, leaf = keccak256(0x00 || data),
// node = keccak256(0x01 || left || right), Z(0) = 0x00..00, Z(i+1) = node(Z(i), Z(i)); a proof is exactly 32
// siblings, bottom-up; bit i of the position set means the running node is the RIGHT child at level i.
// Written from the two documents, independently of the engine and of the vault. UNAUDITED prototype.
import { keccak256, hex, unhex, concat } from './keccak.mjs';

export const DEPTH = 32;
const enc = new TextEncoder();
export const leafHash = (data) => keccak256(concat(Uint8Array.of(0), data));
export const nodeHash = (l, r) => keccak256(concat(Uint8Array.of(1), l, r));
export const ZERO = (() => { const z = [new Uint8Array(32)]; for (let i = 0; i < DEPTH; i++) z.push(nodeHash(z[i], z[i])); return z; })();

const be = (v, n) => {
  let x = BigInt(v);
  if (x < 0n) x += 1n << BigInt(8 * n);   // two's complement
  if (x < 0n || x >> BigInt(8 * n) !== 0n) throw new Error(`value does not fit ${n} bytes`);
  const b = new Uint8Array(n);
  for (let i = n - 1; i >= 0; i--) { b[i] = Number(x & 255n); x >>= 8n; }
  return b;
};

/** positionsHash = keccak256(be32(count) || [be32 marketId || be64 size || be128 openNotional || be64 isoMargin || u8 isolated]...), ascending market id. */
export function positionsHash(positions) {
  const sorted = [...positions].sort((a, b) => a.marketId - b.marketId);
  return keccak256(concat(be(sorted.length, 4), ...sorted.map((p) => concat(be(p.marketId, 4), be(p.size, 8), be(p.openNotional, 16), be(p.isoMargin ?? 0, 8), Uint8Array.of(p.isolated ? 1 : 0)))));
}
/** "PPv1acct" || account:20 || be32 index || be128 balance (two's complement) || be64 escapeValue || positionsHash:32 = 88 bytes. */
export function encodeAccountLeaf({ account, index, balance, escapeValue, positionsHash: ph }) {
  const out = concat(enc.encode('PPv1acct'), unhex(account), be(index, 4), be(balance, 16), be(escapeValue, 8), typeof ph === 'string' ? unhex(ph) : ph);
  if (out.length !== 88) throw new Error(`account leaf is ${out.length} bytes`);
  return out;
}
/** Strict inverse of encodeAccountLeaf (decimal strings; balance signed). */
export function decodeAccountLeaf(bytes) {
  if (bytes.length !== 88 || new TextDecoder().decode(bytes.slice(0, 8)) !== 'PPv1acct') throw new Error('not an account leaf');
  const u = (from, n) => BigInt(hex(bytes.slice(from, from + n)));
  let balance = u(32, 16);
  if (balance >> 127n) balance -= 1n << 128n;
  return { account: hex(bytes.slice(8, 28)), index: Number(u(28, 4)), balance: String(balance), escapeValue: String(u(48, 8)), positionsHash: hex(bytes.slice(56, 88)) };
}
/** "PPv1wdrl" || be64 withdrawalId || account:20 || recipient:20 || be64 amount || be64 height = 72 bytes. */
export function encodeWithdrawalLeaf({ withdrawalId, account, recipient, amount, height }) {
  return concat(enc.encode('PPv1wdrl'), be(withdrawalId, 8), unhex(account), unhex(recipient), be(amount, 8), be(height, 8));
}

/** Sparse depth-32 tree over `leaves`: Map(position -> 32-byte leaf hash). */
export function tree32(leaves) {
  let level = new Map(leaves);
  const levels = [level];
  for (let d = 0; d < DEPTH; d++) {
    const next = new Map();
    for (const i of level.keys()) {
      const p = Math.floor(i / 2);
      if (next.has(p)) continue;
      next.set(p, nodeHash(level.get(p * 2) ?? ZERO[d], level.get(p * 2 + 1) ?? ZERO[d]));
    }
    level = next;
    levels.push(level);
  }
  const root = hex(level.get(0) ?? ZERO[DEPTH]);
  const proof = (index) => {
    const out = [];
    let i = index;
    // positions go up to 2^32 - 1: no 32-bit bitwise operators here (i ^ 1 would wrap negative)
    for (let d = 0; d < DEPTH; d++) { out.push(hex(levels[d].get(i % 2 === 0 ? i + 1 : i - 1) ?? ZERO[d])); i = Math.floor(i / 2); }
    return out;
  };
  return { root, proof };
}
/** Walks `siblings` (exactly 32) from a leaf HASH at `index`; returns the root hex. */
export function root32(leaf, index, siblings) {
  if (!Array.isArray(siblings) || siblings.length !== DEPTH) throw new Error(`a proof is exactly ${DEPTH} siblings`);
  let h = typeof leaf === 'string' ? unhex(leaf) : leaf;
  let i = BigInt(index);
  if (i < 0n || i >> 32n !== 0n) throw new Error('position out of range');
  for (let d = 0; d < DEPTH; d++) {
    const s = unhex(siblings[d]);
    h = (i >> BigInt(d)) & 1n ? nodeHash(s, h) : nodeHash(h, s);
  }
  return hex(h);
}
