// Unit tests of the rehearsal's own arithmetic and key derivations (a wrong helper would make a check vacuous).
// UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { tradeFee, notionalUsdc, fmtUsdc, onTick } from '../rehearsal/util.mjs';
import { scale, usdc, px } from '../lib/client.mjs';
import { nodeDevCommittee, nodeDevKey, userKey, user } from '../lib/keys.mjs';
import { CAST } from '../rehearsal/accounts.mjs';
import { hex } from '../../node/mock/lib/keccak.mjs';

test('taker fee reproduces SPEC section 15 (0.5 BTC at 67,250, 450 ppm = 15.131250 USDC)', () => {
  assert.equal(tradeFee('67250', '0.5', 450), 15131250n);
  assert.equal(notionalUsdc('67250', '0.5'), 33625000000n);
});
test('fees round UP (toward the protocol, S-0.3): one unit above an inexact quotient, exact when it divides', () => {
  assert.equal(tradeFee('0.00000001', '0.00000001', 450), 1n);      // any positive fee is at least one unit
  assert.equal(tradeFee('67317', '0.2', 450), 6058530n);            // 13463.4 * 0.00045 = 6.05853 exactly
  assert.equal(tradeFee('67317', '0.2', 150), 2019510n);
  assert.equal(tradeFee('67317.5', '0.0001', 450), 3030n);          // 6.73175 * 0.00045 = 0.0030292875 -> 0.003030
});
test('scale refuses a value that is not exact at the scale (S-1.2) and keeps the sign', () => {
  assert.equal(usdc('1.000001'), 1000001n);
  assert.equal(usdc('-2.5'), -2500000n);
  assert.throws(() => usdc('1.0000001'));
  assert.throws(() => scale('1e3', 6));
  assert.throws(() => scale('', 6));
});
test('fmtUsdc is the inverse of usdc', () => {
  for (const s of ['0.000000', '1.000001', '-2.500000', '400000.000000']) assert.equal(fmtUsdc(usdc(s)), s);
});
test('onTick lands on the tick, on the side toward zero, and round-trips through px', () => {
  const m = { tickSize: '0.5' };
  assert.equal(onTick(m, '67250', 10), '67317');      // 67317.25 -> 67317.0
  assert.equal(onTick(m, '67250', -10), '67182.5');   // 67182.75 -> 67182.5
  assert.equal(px(onTick(m, '67250', 10)) % px('0.5'), 0n);
  assert.equal(onTick({ tickSize: '0.01' }, '182.4', 0), '182.4');
});
test('the node dev committee derivation is sorted, distinct and stable; user keys differ per label', () => {
  const c = nodeDevCommittee('e2e-r2', 'validator', 7);
  assert.equal(new Set(c.map((k) => k.address)).size, 7);
  assert.ok(c.every((k, i) => i === 0 || k.address > c[i - 1].address));
  assert.notEqual(hex(nodeDevKey('e2e-r2/validator', 0)), hex(nodeDevKey('e2e-r2/oracle', 0)));
  assert.notEqual(hex(userKey('alice')), hex(userKey('bob')));
  assert.equal(user('alice').address, CAST.find((a) => a.label === 'alice').address);
  assert.equal(new Set(CAST.map((a) => a.address)).size, CAST.length);
});
