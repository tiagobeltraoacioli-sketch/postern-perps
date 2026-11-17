// SPEC 3.2 input encoding: round trip and strict refusals. UNAUDITED prototype.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { encodeInputs, decodeInputs } from '../lib/encoding.mjs';
import { hex, unhex } from '../lib/keccak.mjs';

const FX = JSON.parse(readFileSync(new URL('../fixtures/eip712.json', import.meta.url)));
const sig = '0x' + '11'.repeat(64) + '1b';
const inputs = [
  { kind: 'oracle', payload: { round: '598292802', observations: [{ signer: 0, timeMs: '1794878406000', prices: [{ marketId: 1, price: '6725000000000', sources: 5 }], signature: sig }, { signer: 3, timeMs: '1794878406100', prices: [], signature: sig }] } },
  { kind: 'deposit', payload: { depositIndex: '0', account: '0x' + 'ab'.repeat(20), amount: '5000000000' } },
  { kind: 'forced', payload: { forcedIndex: '0', account: '0x' + 'cd'.repeat(20), kind: 2, deadline: '1794881800000', arg: '1000000' } },
  { kind: 'action', type: 'ApproveAgent', message: { account: '0x' + '01'.repeat(20), nonce: '1794878406000', agent: '0x' + '02'.repeat(20), name: 'bot é', expiresAt: '1797392000000' }, signature: sig },
  { kind: 'action', type: 'PlaceOrder', message: { account: '0x' + '01'.repeat(20), nonce: '1794878406001', marketId: '1', isBuy: true, orderType: '0', timeInForce: '2', price: '6725000000000', size: '25000000', reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) }, signature: sig },
];

test('round trip of every input kind and an action with a UTF-8 string', () => {
  const b = encodeInputs(inputs, FX);
  const d = decodeInputs(b, FX);
  assert.equal(d.length, inputs.length);
  assert.deepEqual(d[3].message, inputs[3].message);
  assert.deepEqual(d[4].message, inputs[4].message);
  assert.deepEqual(d[0].payload, inputs[0].payload);
  assert.deepEqual(d[2].payload, inputs[2].payload);
  assert.equal(hex(encodeInputs(d, FX)), hex(b), 'canonical: re-encoding the decoded form gives the same bytes');
  // Layout of the first bytes: be32(count) then tag 1.
  assert.equal(hex(b.slice(0, 5)), '0x0000000501');
});

test('strict decoder refuses trailing bytes, truncation, tag disorder, bool > 1 and unknown tags', () => {
  const b = encodeInputs(inputs, FX);
  const plus = new Uint8Array(b.length + 1);
  plus.set(b);
  assert.throws(() => decodeInputs(plus, FX), /trailing/);
  assert.throws(() => decodeInputs(b.slice(0, b.length - 1), FX), /truncated|trailing/);
  assert.throws(() => encodeInputs([inputs[1], inputs[0]], FX), /BLOCK_ORDER/);
  assert.throws(() => encodeInputs([inputs[0], inputs[0]], FX), /BLOCK_ORDER/);
  const one = encodeInputs([inputs[4]], FX);
  // PlaceOrder: tag(1) len(4) typeId(1) account(20) nonce(8) marketId(4) isBuy(1) at offset 4+1+4+1+20+8+4 = 42
  const badBool = one.slice();
  badBool[42] = 2;
  assert.throws(() => decodeInputs(badBool, FX), /bool/);
  const badTag = one.slice();
  badTag[4] = 9;
  assert.throws(() => decodeInputs(badTag, FX), /unknown tag/);
  assert.throws(() => encodeInputs([{ ...inputs[1], payload: { ...inputs[1].payload, amount: String(1n << 64n) } }], FX), /range/);
  void unhex;
});
