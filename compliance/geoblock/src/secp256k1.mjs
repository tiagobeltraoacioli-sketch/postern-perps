// secp256k1 public-key RECOVERY only (no signing, no secret keys). UNAUDITED prototype, no dependencies.
// Inputs are public (a digest and a signature), so constant time is not required. Every malformed input is
// refused (returns null) rather than coerced: 65 bytes r||s||v, v in {27,28}, 1 <= r,s < n, s <= n/2
// (low-s, the EIP-2 rule), r must be the x of a curve point.

import { keccak256, bytesToHex } from './keccak.mjs';

const P = 0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2fn;
const N = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n;
const HALF_N = N >> 1n;
const GX = 0x79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798n;
const GY = 0x483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8n;

const mod = (a, m = P) => { const r = a % m; return r >= 0n ? r : r + m; };
function powmod(b, e, m) {
  let r = 1n; b = mod(b, m);
  while (e > 0n) { if (e & 1n) r = (r * b) % m; b = (b * b) % m; e >>= 1n; }
  return r;
}
const inv = (a, m = P) => powmod(a, m - 2n, m); // m is prime for both P and N

// Jacobian points [X, Y, Z]; Z = 0 is infinity.
const INF = [0n, 1n, 0n];
function dbl([X, Y, Z]) {
  if (Z === 0n || Y === 0n) return INF;
  const S = mod(4n * X * Y * Y);
  const M = mod(3n * X * X);
  const X3 = mod(M * M - 2n * S);
  const Y3 = mod(M * (S - X3) - 8n * Y * Y * Y * Y);
  const Z3 = mod(2n * Y * Z);
  return [X3, Y3, Z3];
}
function add(p, q) {
  if (p[2] === 0n) return q;
  if (q[2] === 0n) return p;
  const [X1, Y1, Z1] = p; const [X2, Y2, Z2] = q;
  const Z1Z1 = mod(Z1 * Z1); const Z2Z2 = mod(Z2 * Z2);
  const U1 = mod(X1 * Z2Z2); const U2 = mod(X2 * Z1Z1);
  const S1 = mod(Y1 * Z2 * Z2Z2); const S2 = mod(Y2 * Z1 * Z1Z1);
  if (U1 === U2) return S1 === S2 ? dbl(p) : INF;
  const H = mod(U2 - U1); const R = mod(S2 - S1);
  const HH = mod(H * H); const HHH = mod(H * HH);
  const X3 = mod(R * R - HHH - 2n * U1 * HH);
  const Y3 = mod(R * (U1 * HH - X3) - S1 * HHH);
  const Z3 = mod(H * Z1 * Z2);
  return [X3, Y3, Z3];
}
function mul(p, k) {
  let r = INF; let a = p;
  while (k > 0n) { if (k & 1n) r = add(r, a); a = dbl(a); k >>= 1n; }
  return r;
}
function toAffine([X, Y, Z]) {
  if (Z === 0n) return null;
  const zi = inv(Z); const zi2 = mod(zi * zi);
  return [mod(X * zi2), mod(Y * zi2 * zi)];
}

const be = (bytes) => { let v = 0n; for (const b of bytes) v = (v << 8n) | BigInt(b); return v; };
const to32 = (v) => { const o = new Uint8Array(32); for (let i = 31; i >= 0; i--) { o[i] = Number(v & 0xffn); v >>= 8n; } return o; };

/**
 * Parse and check a 65-byte Ethereum signature. Returns {r, s, recId} or null.
 * Rules (identical to the node's admission proposal of agent 389 and the bridge's _checkSignatures):
 * exactly 65 bytes, v in {27, 28}, 1 <= r < n, 1 <= s <= n/2.
 */
export function parseSignature(sig) {
  if (!(sig instanceof Uint8Array) || sig.length !== 65) return null;
  const r = be(sig.subarray(0, 32)); const s = be(sig.subarray(32, 64)); const v = sig[64];
  if (v !== 27 && v !== 28) return null;
  if (r < 1n || r >= N) return null;
  if (s < 1n || s > HALF_N) return null;
  return { r, s, recId: v - 27 };
}

/** Recover the signer address (lowercase 0x hex) of a 32-byte digest, or null when anything is invalid. */
export function recoverAddress(digest, sig) {
  if (!(digest instanceof Uint8Array) || digest.length !== 32) return null;
  const p = parseSignature(sig);
  if (!p) return null;
  const { r, s, recId } = p;
  // x = r (r < n < p, and with v in {27,28} only the r < n candidate is allowed, as in Ethereum).
  const x = r;
  const y2 = mod(x * x * x + 7n);
  let y = powmod(y2, (P + 1n) / 4n, P);
  if (mod(y * y) !== y2) return null; // r is not the x of a point
  if ((y & 1n) !== BigInt(recId)) y = P - y;
  const R = [x, y, 1n];
  const e = mod(be(digest), N);
  const ri = inv(r, N);
  const u1 = mod(-e * ri, N);
  const u2 = mod(s * ri, N);
  const Q = add(mul([GX, GY, 1n], u1), mul(R, u2));
  const aff = toAffine(Q);
  if (!aff) return null;
  const pub = new Uint8Array(64); pub.set(to32(aff[0]), 0); pub.set(to32(aff[1]), 32);
  return bytesToHex(keccak256(pub).subarray(12));
}

export const SECP256K1_N = N;
