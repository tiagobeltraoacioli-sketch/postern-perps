// secp256k1 sign / recover with BigInt (Jacobian coordinates). Dependency-free.
// MOCK AND TEST USE ONLY: not constant time, never use with a real key.
import { keccak256, concat, hex } from './keccak.mjs';
const P = 0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2fn;
export const N = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n;
const G = [0x79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798n, 0x483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8n, 1n];
const mod = (a, m = P) => ((a % m) + m) % m;
function inv(a, m) {
  let [r0, r1, s0, s1] = [mod(a, m), m, 1n, 0n];
  while (r1 !== 0n) { const q = r0 / r1; [r0, r1] = [r1, r0 - q * r1]; [s0, s1] = [s1, s0 - q * s1]; }
  if (r0 !== 1n) throw new Error('no inverse');
  return mod(s0, m);
}
function dbl([X, Y, Z]) {
  if (Y === 0n) return [0n, 1n, 0n];
  const A = mod(X * X), B = mod(Y * Y), C = mod(B * B);
  const D = mod(2n * (mod((X + B) * (X + B)) - A - C)), E = mod(3n * A), F = mod(E * E);
  const X3 = mod(F - 2n * D);
  return [X3, mod(E * (D - X3) - 8n * C), mod(2n * Y * Z)];
}
function add(p, q) {
  if (p[2] === 0n) return q;
  if (q[2] === 0n) return p;
  const [X1, Y1, Z1] = p, [X2, Y2, Z2] = q;
  const Z1Z1 = mod(Z1 * Z1), Z2Z2 = mod(Z2 * Z2);
  const U1 = mod(X1 * Z2Z2), U2 = mod(X2 * Z1Z1), S1 = mod(Y1 * Z2 * Z2Z2), S2 = mod(Y2 * Z1 * Z1Z1);
  if (U1 === U2) return S1 === S2 ? dbl(p) : [0n, 1n, 0n];
  const H = mod(U2 - U1), R = mod(S2 - S1), HH = mod(H * H), HHH = mod(H * HH), V = mod(U1 * HH);
  const X3 = mod(R * R - HHH - 2n * V);
  return [X3, mod(R * (V - X3) - S1 * HHH), mod(H * Z1 * Z2)];
}
function mulPlain(k, p) {
  let r = [0n, 1n, 0n];
  for (let i = k.toString(2).length - 1; i >= 0; i--) { r = dbl(r); if ((k >> BigInt(i)) & 1n) r = add(r, p); }
  return r;
}
// Fixed-base table for G (4-bit windows: TABLE[w][j] = j * 16^w * G), built once on first use: k*G costs at most
// 64 additions and no doublings. Equality with the plain double-and-add is tested in test/crypto.test.mjs.
let TABLE = null;
function table() {
  if (TABLE) return TABLE;
  TABLE = [];
  let base = G;
  for (let w = 0; w < 64; w++) {
    const row = [[0n, 1n, 0n]];
    for (let j = 1; j < 16; j++) row.push(add(row[j - 1], base));
    TABLE.push(row);
    base = dbl(dbl(dbl(dbl(base))));
  }
  return TABLE;
}
function mulG(k) {
  const t = table();
  let r = [0n, 1n, 0n];
  for (let w = 0; w < 64 && k > 0n; w++, k >>= 4n) {
    const j = Number(k & 15n);
    if (j) r = add(r, t[w][j]);
  }
  return r;
}
function mul(k, p) {
  if (p === G) return mulG(k);
  // 4-bit fixed window for an arbitrary point.
  const pre = [[0n, 1n, 0n], p];
  for (let j = 2; j < 16; j++) pre.push(add(pre[j - 1], p));
  const digits = k.toString(16);
  let r = [0n, 1n, 0n];
  for (const ch of digits) {
    r = dbl(dbl(dbl(dbl(r))));
    const j = parseInt(ch, 16);
    if (j) r = add(r, pre[j]);
  }
  return r;
}
export const _testOnly = { mulPlain, mul, G, affine };
function affine([X, Y, Z]) {
  if (Z === 0n) throw new Error('point at infinity');
  const zi = inv(Z, P), zi2 = mod(zi * zi);
  return [mod(X * zi2), mod(Y * zi2 * zi)];
}
const toBytes = (x, n = 32) => new Uint8Array(Buffer.from(x.toString(16).padStart(n * 2, '0'), 'hex'));
const toBig = (b) => BigInt('0x' + (Buffer.from(b).toString('hex') || '0'));
function pubToAddress([x, y]) { return hex(keccak256(concat(toBytes(x), toBytes(y))).slice(12)); }
function checkKey(priv) {
  if (!(priv instanceof Uint8Array) || priv.length !== 32) throw new Error('key must be 32 bytes');
  const d = toBig(priv);
  if (d === 0n || d >= N) throw new Error('key out of range');
  return d;
}
export function addressOf(priv) { return pubToAddress(affine(mul(checkKey(priv), G))); }
// Deterministic nonce (hash of key and message; fine for throwaway keys, NOT RFC 6979).
export function sign(hash, priv) {
  if (!(hash instanceof Uint8Array) || hash.length !== 32) throw new Error('hash must be 32 bytes');
  const d = checkKey(priv), z = toBig(hash);
  for (let ctr = 0; ; ctr++) {
    const k = mod(toBig(keccak256(concat(priv, hash, Uint8Array.of(ctr)))), N);
    if (k === 0n) continue;
    const [rx, ry] = affine(mul(k, G));
    const r = mod(rx, N);
    if (r === 0n || rx >= N) continue;
    let s = mod(inv(k, N) * (z + r * d), N);
    if (s === 0n) continue;
    let v = Number(ry & 1n);
    if (s > N / 2n) { s = N - s; v ^= 1; }
    return concat(toBytes(r), toBytes(s), Uint8Array.of(27 + v));
  }
}
function sqrtP(a) {
  let r = 1n, b = mod(a), e = (P + 1n) / 4n;
  while (e > 0n) { if (e & 1n) r = mod(r * b); b = mod(b * b); e >>= 1n; }
  return r;
}
// Returns the 0x address that signed `hash`, or throws. The rule of the API (docs/API.md section 7, the vault's
// `_checkSignatures` pattern and the frontend's pin): exactly 65 bytes r||s||v, v in {27, 28} only (0/1 refused:
// agent 393 narrowed 351's draft, which accepted 0/1, so one signature has exactly one accepted encoding),
// r and s in [1, n-1], s <= n/2 (high-s refused: no malleated second form).
export function recover(hash, sig) {
  if (!(sig instanceof Uint8Array) || sig.length !== 65) throw new Error('signature must be 65 bytes');
  if (!(hash instanceof Uint8Array) || hash.length !== 32) throw new Error('hash must be 32 bytes');
  const r = toBig(sig.slice(0, 32)), s = toBig(sig.slice(32, 64));
  if (sig[64] !== 27 && sig[64] !== 28) throw new Error('bad v (27 or 28 only)');
  const v = sig[64] - 27;
  if (r === 0n || r >= N || s === 0n || s > N / 2n) throw new Error('bad r or s (high-s refused)');
  const y2 = mod(r * r * r + 7n);
  let y = sqrtP(y2);
  if (mod(y * y) !== y2) throw new Error('r not on curve');
  if (Number(y & 1n) !== v) y = P - y;
  const ri = inv(r, N), z = toBig(hash);
  const q = add(mul(mod(-z * ri, N), G), mul(mod(s * ri, N), [r, y, 1n]));
  return pubToAddress(affine(q));
}
