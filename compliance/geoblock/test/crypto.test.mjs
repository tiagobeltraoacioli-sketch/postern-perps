import test from 'node:test';
import assert from 'node:assert/strict';
import { keccak256, utf8, bytesToHex, hexToBytes, toChecksumAddress, normalizeEvmAddress } from '../src/keccak.mjs';
import { sha256, hmacSha256, hex, timingSafeEqual } from '../src/sha256.mjs';
import { recoverAddress, parseSignature, SECP256K1_N } from '../src/secp256k1.mjs';
import { readJson } from './helpers.mjs';

test('keccak256 known answers', () => {
  assert.equal(bytesToHex(keccak256(new Uint8Array())), '0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470');
  assert.equal(bytesToHex(keccak256(utf8('abc'))), '0x4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45');
});

test('keccak256 matches an independent implementation across block boundaries', () => {
  const { keccakVectors } = readJson('vectors/attestation-vectors.json');
  assert.equal(keccakVectors.length, 9);
  for (const v of keccakVectors) assert.equal(bytesToHex(keccak256(hexToBytes(v.input))), v.keccak256, `len ${(v.input.length - 2) / 2}`);
});

test('EIP-55 vectors (from the EIP)', () => {
  for (const a of ['0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed', '0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359',
    '0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB', '0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb']) {
    assert.equal(toChecksumAddress(a.toLowerCase()), a);
    assert.equal(normalizeEvmAddress(a), a.toLowerCase());
  }
  assert.equal(normalizeEvmAddress('0x5aaeb6053F3E94C9b9A09f33669435E7Ef1BeAed'), null, 'one wrong-case letter');
  assert.equal(normalizeEvmAddress('0x5AAEB6053F3E94C9B9A09F33669435E7EF1BEAED'), '0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed', 'all upper accepted');
  for (const bad of ['5aaeb6053f3e94c9b9a09f33669435e7ef1beaed', '0x5aaeb6053f3e94c9b9a09f33669435e7ef1beae', '0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaedd', '0xg aaeb6053f3e94c9b9a09f33669435e7ef1bea', null, 7, ' 0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed']) {
    assert.equal(normalizeEvmAddress(bad), null, String(bad));
  }
});

test('sha256 NIST answers', () => {
  assert.equal(hex(sha256(new Uint8Array())), 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
  assert.equal(hex(sha256(utf8('abc'))), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  assert.equal(hex(sha256(utf8('abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq'))), '248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1');
  assert.equal(hex(sha256(utf8('a'.repeat(1000000)))), 'cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0');
});

test('HMAC-SHA256 RFC 4231 cases 1, 2, 6', () => {
  assert.equal(hex(hmacSha256(new Uint8Array(20).fill(0x0b), utf8('Hi There'))), 'b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7');
  assert.equal(hex(hmacSha256(utf8('Jefe'), utf8('what do ya want for nothing?'))), '5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843');
  assert.equal(hex(hmacSha256(new Uint8Array(131).fill(0xaa), utf8('Test Using Larger Than Block-Size Key - Hash Key First'))), '60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54');
});

test('timingSafeEqual', () => {
  assert.equal(timingSafeEqual(utf8('ab'), utf8('ab')), true);
  assert.equal(timingSafeEqual(utf8('ab'), utf8('ac')), false);
  assert.equal(timingSafeEqual(utf8('ab'), utf8('abc')), false);
});

test('secp256k1 recovery matches the independent vectors and refuses malformed signatures', () => {
  const { vectors, accounts } = readJson('vectors/attestation-vectors.json');
  const valid = vectors.find((v) => v.name === 'valid');
  assert.equal(recoverAddress(hexToBytes(valid.digest), hexToBytes(valid.signature)), accounts[0]);
  const sig = hexToBytes(valid.signature);
  assert.equal(recoverAddress(hexToBytes(valid.digest), sig.subarray(0, 64)), null, '64 bytes');
  assert.equal(recoverAddress(hexToBytes(valid.digest).subarray(0, 31), sig), null, '31-byte digest');
  const s0 = sig.slice(); s0.fill(0, 32, 64); assert.equal(parseSignature(s0), null, 's = 0');
  const rn = sig.slice(); rn.set(hexToBytes('0x' + SECP256K1_N.toString(16)), 0); assert.equal(parseSignature(rn), null, 'r = n');
  const v29 = sig.slice(); v29[64] = 29; assert.equal(parseSignature(v29), null, 'v = 29');
  // r that is not the x coordinate of any point: x = 5 has no square root of x^3+7 mod p? search a small one
  let found = false;
  for (let x = 1n; x < 50n && !found; x++) {
    const P = 0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2fn;
    const y2 = (x * x * x + 7n) % P; let r = 1n; let b = y2; let e = (P + 1n) / 4n;
    while (e > 0n) { if (e & 1n) r = (r * b) % P; b = (b * b) % P; e >>= 1n; }
    if ((r * r) % P !== y2) {
      const bad = sig.slice(); bad.fill(0, 0, 32); bad[31] = Number(x);
      assert.equal(recoverAddress(hexToBytes(valid.digest), bad), null, `r = ${x} not on curve`);
      found = true;
    }
  }
  assert.ok(found);
});
