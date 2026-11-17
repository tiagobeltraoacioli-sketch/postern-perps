// Keccak-256 (the Ethereum variant: original Keccak padding 0x01, not SHA3's 0x06).
// UNAUDITED prototype. No dependencies. Used only for public data (EIP-55 checksums, EIP-712 digests of
// attestation records), never for secrets, so constant time is not a requirement here.

const MASK = (1n << 64n) - 1n;
const RC = [
  0x0000000000000001n, 0x0000000000008082n, 0x800000000000808an, 0x8000000080008000n,
  0x000000000000808bn, 0x0000000080000001n, 0x8000000080008081n, 0x8000000000008009n,
  0x000000000000008an, 0x0000000000000088n, 0x0000000080008009n, 0x000000008000000an,
  0x000000008000808bn, 0x800000000000008bn, 0x8000000000008089n, 0x8000000000008003n,
  0x8000000000008002n, 0x8000000000000080n, 0x000000000000800an, 0x800000008000000an,
  0x8000000080008081n, 0x8000000000008080n, 0x0000000080000001n, 0x8000000080008008n,
];
// Rotation offsets r[x + 5y].
const ROT = [0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14];

function rotl(v, n) {
  if (n === 0) return v;
  const b = BigInt(n);
  return ((v << b) | (v >> (64n - b))) & MASK;
}

function keccakF(s) {
  const c = new Array(5);
  const b = new Array(25);
  for (let round = 0; round < 24; round++) {
    for (let x = 0; x < 5; x++) c[x] = s[x] ^ s[x + 5] ^ s[x + 10] ^ s[x + 15] ^ s[x + 20];
    for (let x = 0; x < 5; x++) {
      const d = c[(x + 4) % 5] ^ rotl(c[(x + 1) % 5], 1);
      for (let y = 0; y < 25; y += 5) s[x + y] ^= d;
    }
    for (let x = 0; x < 5; x++) {
      for (let y = 0; y < 5; y++) {
        b[y + 5 * ((2 * x + 3 * y) % 5)] = rotl(s[x + 5 * y], ROT[x + 5 * y]);
      }
    }
    for (let x = 0; x < 5; x++) {
      for (let y = 0; y < 25; y += 5) {
        s[x + y] = b[x + y] ^ (~b[((x + 1) % 5) + y] & MASK & b[((x + 2) % 5) + y]);
      }
    }
    s[0] ^= RC[round];
  }
}

/** keccak256(bytes: Uint8Array) -> Uint8Array(32) */
export function keccak256(data) {
  if (!(data instanceof Uint8Array)) throw new TypeError('keccak256: Uint8Array required');
  const rate = 136;
  const padded = new Uint8Array(Math.floor(data.length / rate) * rate + rate);
  padded.set(data);
  padded[data.length] ^= 0x01;
  padded[padded.length - 1] ^= 0x80;
  const s = new Array(25).fill(0n);
  for (let off = 0; off < padded.length; off += rate) {
    for (let i = 0; i < rate / 8; i++) {
      let lane = 0n;
      for (let k = 7; k >= 0; k--) lane = (lane << 8n) | BigInt(padded[off + i * 8 + k]);
      s[i] ^= lane;
    }
    keccakF(s);
  }
  const out = new Uint8Array(32);
  for (let i = 0; i < 4; i++) {
    let lane = s[i];
    for (let k = 0; k < 8; k++) { out[i * 8 + k] = Number(lane & 0xffn); lane >>= 8n; }
  }
  return out;
}

export function utf8(str) { return new TextEncoder().encode(str); }

export function hexToBytes(hex) {
  if (typeof hex !== 'string' || !/^0x([0-9a-fA-F]{2})*$/.test(hex)) throw new TypeError('hexToBytes: 0x-prefixed even-length hex required');
  const out = new Uint8Array((hex.length - 2) / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(2 + 2 * i, 4 + 2 * i), 16);
  return out;
}

export function bytesToHex(bytes) {
  let s = '0x';
  for (const b of bytes) s += b.toString(16).padStart(2, '0');
  return s;
}

export function concatBytes(...parts) {
  const n = parts.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(n);
  let o = 0;
  for (const p of parts) { out.set(p, o); o += p.length; }
  return out;
}

/** EIP-55 checksum form of a 20-byte address given as 0x + 40 hex (any case). */
export function toChecksumAddress(addr) {
  if (!/^0x[0-9a-fA-F]{40}$/.test(addr)) throw new TypeError('not a 20-byte hex address');
  const lower = addr.slice(2).toLowerCase();
  const h = bytesToHex(keccak256(utf8(lower))).slice(2);
  let out = '0x';
  for (let i = 0; i < 40; i++) out += parseInt(h[i], 16) >= 8 ? lower[i].toUpperCase() : lower[i];
  return out;
}

/**
 * Normalize an EVM address for screening: returns the lowercase form, or null when the input is not a
 * 20-byte hex address or is mixed-case with a wrong EIP-55 checksum (a mistyped address is never silently
 * accepted as some other address).
 */
export function normalizeEvmAddress(addr) {
  if (typeof addr !== 'string' || !/^0x[0-9a-fA-F]{40}$/.test(addr)) return null;
  const body = addr.slice(2);
  const allLower = body === body.toLowerCase();
  const allUpper = body === body.toUpperCase();
  if (!allLower && !allUpper && toChecksumAddress(addr) !== addr) return null;
  return '0x' + body.toLowerCase();
}
