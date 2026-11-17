// Deterministic pseudo-random numbers for the mock: SplitMix64 on BigInt. Same seed, same sequence, on every
// machine. Never used for keys or anything secret. UNAUDITED prototype, mock/test use only.
const M64 = (1n << 64n) - 1n;

export class Rng {
  constructor(seed) {
    this.s = BigInt.asUintN(64, BigInt(seed));
  }
  /** Next 64-bit value. */
  next() {
    this.s = (this.s + 0x9e3779b97f4a7c15n) & M64;
    let z = this.s;
    z = ((z ^ (z >> 30n)) * 0xbf58476d1ce4e5b9n) & M64;
    z = ((z ^ (z >> 27n)) * 0x94d049bb133111ebn) & M64;
    return z ^ (z >> 31n);
  }
  /** Uniform integer in [0, n) for a BigInt n > 0 (rejection sampling: no modulo bias). */
  below(n) {
    if (n <= 0n) throw new Error('n must be positive');
    const limit = M64 - ((M64 + 1n) % n);
    for (;;) {
      const x = this.next();
      if (x <= limit) return x % n;
    }
  }
  /** Uniform integer in [lo, hi] (BigInt). */
  range(lo, hi) {
    return lo + this.below(hi - lo + 1n);
  }
  /** True with probability num/den. */
  chance(num, den) {
    return this.below(BigInt(den)) < BigInt(num);
  }
  /** A child generator for an independent stream (per market, per account). */
  fork(label) {
    let h = this.s;
    for (const c of String(label)) h = BigInt.asUintN(64, (h ^ BigInt(c.charCodeAt(0))) * 0x100000001b3n);
    return new Rng(h);
  }
}
