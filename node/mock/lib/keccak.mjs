// keccak256 (the pre-NIST padding used by Ethereum). Dependency-free. Mock/test use only.
// Keccak-f[1600] on 32-bit lane halves (lo, hi) in a Uint32Array: about a thousand times faster than BigInt
// lanes (agent 393 replaced 351's BigInt version, which took about 2 ms per hash on the Mac). Checked against the
// known vectors and against viem's keccak256 on random inputs of every length 0..300 in test/crypto.test.mjs.
const RC = new Uint32Array([
  0x00000001, 0x00000000, 0x00008082, 0x00000000, 0x0000808a, 0x80000000, 0x80008000, 0x80000000,
  0x0000808b, 0x00000000, 0x80000001, 0x00000000, 0x80008081, 0x80000000, 0x00008009, 0x80000000,
  0x0000008a, 0x00000000, 0x00000088, 0x00000000, 0x80008009, 0x00000000, 0x8000000a, 0x00000000,
  0x8000808b, 0x00000000, 0x0000008b, 0x80000000, 0x00008089, 0x80000000, 0x00008003, 0x80000000,
  0x00008002, 0x80000000, 0x00000080, 0x80000000, 0x0000800a, 0x00000000, 0x8000000a, 0x80000000,
  0x80008081, 0x80000000, 0x00008080, 0x80000000, 0x80000001, 0x00000000, 0x80008008, 0x80000000]);
const ROT = [0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14];
// PI[i] = destination lane of lane i (b[y + ((2x + 3y) % 5) * 5] = rot(s[x + 5y])).
const PI = new Array(25);
for (let x = 0; x < 5; x++) for (let y = 0; y < 5; y++) PI[x + 5 * y] = y + ((2 * x + 3 * y) % 5) * 5;
const C = new Uint32Array(10), B = new Uint32Array(50);
function f(s) {
  for (let r = 0; r < 24; r++) {
    for (let x = 0; x < 5; x++) {
      C[2 * x] = s[2 * x] ^ s[2 * x + 10] ^ s[2 * x + 20] ^ s[2 * x + 30] ^ s[2 * x + 40];
      C[2 * x + 1] = s[2 * x + 1] ^ s[2 * x + 11] ^ s[2 * x + 21] ^ s[2 * x + 31] ^ s[2 * x + 41];
    }
    for (let x = 0; x < 5; x++) {
      const a = ((x + 4) % 5) * 2, b = ((x + 1) % 5) * 2;
      const dl = C[a] ^ ((C[b] << 1) | (C[b + 1] >>> 31));
      const dh = C[a + 1] ^ ((C[b + 1] << 1) | (C[b] >>> 31));
      for (let y = 0; y < 25; y += 5) { s[2 * (x + y)] ^= dl; s[2 * (x + y) + 1] ^= dh; }
    }
    for (let i = 0; i < 25; i++) {
      const lo = s[2 * i], hi = s[2 * i + 1], n = ROT[i], d = 2 * PI[i];
      if (n === 0) { B[d] = lo; B[d + 1] = hi; }
      else if (n < 32) { B[d] = (lo << n) | (hi >>> (32 - n)); B[d + 1] = (hi << n) | (lo >>> (32 - n)); }
      else if (n === 32) { B[d] = hi; B[d + 1] = lo; }
      else { const m = n - 32; B[d] = (hi << m) | (lo >>> (32 - m)); B[d + 1] = (lo << m) | (hi >>> (32 - m)); }
    }
    for (let y = 0; y < 25; y += 5) {
      for (let x = 0; x < 5; x++) {
        const i = 2 * (x + y), j = 2 * (((x + 1) % 5) + y), k = 2 * (((x + 2) % 5) + y);
        s[i] = B[i] ^ (~B[j] & B[k]);
        s[i + 1] = B[i + 1] ^ (~B[j + 1] & B[k + 1]);
      }
    }
    s[0] ^= RC[2 * r];
    s[1] ^= RC[2 * r + 1];
  }
}
export function keccak256(data) {
  if (!(data instanceof Uint8Array)) throw new Error('keccak256 takes a Uint8Array');
  const rate = 136;
  const len = data.length;
  const padded = new Uint8Array(Math.ceil((len + 1) / rate) * rate);
  padded.set(data);
  padded[len] ^= 0x01;
  padded[padded.length - 1] ^= 0x80;
  const s = new Uint32Array(50);
  const dv = new DataView(padded.buffer);
  for (let off = 0; off < padded.length; off += rate) {
    for (let i = 0; i < rate / 4; i++) s[i] ^= dv.getUint32(off + i * 4, true);
    f(s);
  }
  const out = new Uint8Array(32);
  const ov = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) ov.setUint32(i * 4, s[i], true);
  return out;
}
export const hex = (b) => '0x' + Buffer.from(b).toString('hex');
export const unhex = (h) => {
  if (typeof h !== 'string' || !/^0x([0-9a-fA-F]{2})*$/.test(h)) throw new Error('bad hex');
  return new Uint8Array(Buffer.from(h.slice(2), 'hex'));
};
export const concat = (...parts) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of parts) { out.set(p, o); o += p.length; }
  return out;
};
