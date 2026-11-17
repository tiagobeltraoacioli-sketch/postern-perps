// The mock's dependency-free crypto against vectors recorded from viem (an independent implementation;
// regenerate with test/vectors/genvec-viem.mjs). UNAUDITED prototype.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { keccak256, hex, unhex } from '../lib/keccak.mjs';
import { sign, recover, addressOf, N, _testOnly } from '../lib/secp256k1.mjs';
import { typedDataHash } from '../lib/eip712.mjs';

const V = JSON.parse(readFileSync(new URL('./vectors/crypto.json', import.meta.url)));
const FX = JSON.parse(readFileSync(new URL('../fixtures/eip712.json', import.meta.url)));
const enc = new TextEncoder();
const keyOf = (label) => keccak256(enc.encode(label));

test('keccak256 matches viem on 14 lengths around the 136-byte rate', () => {
  assert.equal(V.keccak.length, 14);
  for (const v of V.keccak) assert.equal(hex(keccak256(unhex(v.input))), v.hash, `length ${(v.input.length - 2) / 2}`);
});

test('secp256k1: key 1 gives the well-known address; fixed-base equals double-and-add', () => {
  const one = new Uint8Array(32);
  one[31] = 1;
  assert.equal(addressOf(one), '0x7e5f4552091a69125d5dfcb7b8c2659029395bdf');
  const { mulPlain, mul, G, affine } = _testOnly;
  for (const k of [1n, 2n, 15n, 16n, 17n, 0xffffn, N - 1n, (N >> 1n) + 12345n]) {
    assert.deepEqual(affine(mul(k, G)), affine(mulPlain(k, G)), `k=${k}`);
    const P = mul(7n, G);
    assert.deepEqual(affine(mul(k, P)), affine(mulPlain(k, P)), `k=${k} on 7G`);
  }
});

test('EIP-712 digests and signatures match viem for every action type', () => {
  assert.equal(V.typed.length, Object.keys(FX.types).length, 'one vector per typed action');
  for (const v of V.typed) {
    const digest = typedDataHash(FX.domain, v.type, FX.types[v.type], v.message);
    assert.equal(hex(digest), v.digest, v.type);
    assert.equal(addressOf(keyOf(v.keyLabel)), v.address);
    assert.equal(recover(digest, unhex(v.signature)), v.address, `${v.type} recovers`);
    // Our own signature (deterministic nonce, not RFC 6979: differs from viem's bytes) must recover too and be low-s.
    const ours = sign(digest, keyOf(v.keyLabel));
    assert.equal(recover(digest, ours), v.address);
    assert.ok(BigInt(hex(ours.slice(32, 64))) <= N / 2n);
  }
});

test('the four-field vault domain (verifyingContract) matches viem', () => {
  const c = V.checkpoint;
  assert.equal(hex(typedDataHash(c.domain, 'Checkpoint', c.fields, c.message)), c.digest);
});

test('recover refuses every non-canonical signature form', () => {
  const v = V.typed[0];
  const digest = unhex(v.digest);
  const sig = unhex(v.signature);
  const s = BigInt(hex(sig.slice(32, 64)));
  const high = sig.slice();
  high.set(unhex('0x' + (N - s).toString(16).padStart(64, '0')), 32);
  high[64] = sig[64] === 27 ? 28 : 27;
  assert.throws(() => recover(digest, high), /high-s/);
  for (const bad of [0, 1, 26, 29, 35]) {
    const b = sig.slice();
    b[64] = bad;
    assert.throws(() => recover(digest, b), /bad v/, `v=${bad}`);
  }
  assert.throws(() => recover(digest, sig.slice(0, 64)), /65 bytes/);
  const zeroR = sig.slice();
  zeroR.fill(0, 0, 32);
  assert.throws(() => recover(digest, zeroR), /bad r or s/);
  const bigR = sig.slice();
  bigR.set(unhex('0x' + N.toString(16)), 0);
  assert.throws(() => recover(digest, bigR), /bad r or s/);
  assert.throws(() => addressOf(new Uint8Array(32)), /out of range/);
});

test('EIP-712 encoding refuses non-canonical and out-of-range values', () => {
  const t = FX.types.CancelOrder;
  const base = { account: '0x' + '11'.repeat(20), nonce: '5', marketId: 2, orderId: '1' };
  assert.doesNotThrow(() => typedDataHash(FX.domain, 'CancelOrder', t, base));
  for (const [k, val] of [['nonce', '05'], ['nonce', '-1'], ['nonce', '+5'], ['nonce', '18446744073709551616'], ['nonce', 5.5], ['marketId', '4294967296'], ['account', '0x11'], ['orderId', '-0']]) {
    assert.throws(() => typedDataHash(FX.domain, 'CancelOrder', t, { ...base, [k]: val }), undefined, `${k}=${val}`);
  }
  assert.throws(() => typedDataHash(FX.domain, 'CancelOrder', t, { ...base, extra: 1 }), /unknown field/);
  const { orderId, ...missing } = base;
  assert.throws(() => typedDataHash(FX.domain, 'CancelOrder', t, missing), /missing field/);
  assert.throws(() => typedDataHash({ ...FX.domain, bogus: 1 }, 'CancelOrder', t, base), /unknown domain field/);
});
