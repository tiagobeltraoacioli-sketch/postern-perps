import test from 'node:test';
import assert from 'node:assert/strict';
import { verifyAttestation, attestationDigest, domainSeparator, statementHash, typedData, attestationRecord, ATTESTATION_TYPE } from '../src/attestation.mjs';
import { keccak256, utf8, bytesToHex } from '../src/keccak.mjs';
import { decide } from '../src/decide.mjs';
import { readJson, fixtureList, fixtureSanctions } from './helpers.mjs';

const V = readJson('vectors/attestation-vectors.json');
const statements = { [V.statement.id]: V.statement.hash };

test('domain separator, type hash and statement hash equal the independent implementation', () => {
  assert.equal(bytesToHex(domainSeparator()), V.domainSeparator);
  assert.equal(bytesToHex(keccak256(utf8(ATTESTATION_TYPE))), V.typeHash);
  assert.equal(statementHash(V.statement.text), V.statement.hash);
});

test('attestation vectors (independent signer)', () => {
  assert.equal(V.vectors.length, 11);
  for (const v of V.vectors) {
    assert.equal(bytesToHex(attestationDigest(v.message)), v.digest, `${v.name}: digest`);
    const r = verifyAttestation({ message: v.message, signature: v.signature }, { statements, nowMs: v.nowMs });
    assert.equal(r.status, v.expect.status, `${v.name}: ${r.reason}`);
    if (v.expect.signer) assert.equal(r.signer, v.expect.signer, v.name);
  }
});

test('a statement that is no longer current, or whose text changed, is invalid', () => {
  const v = V.vectors[0];
  assert.equal(verifyAttestation(v, { statements: {}, nowMs: v.nowMs }).reason, 'statement not current');
  assert.equal(verifyAttestation(v, { statements: { [V.statement.id]: statementHash(V.statement.text + ' ') }, nowMs: v.nowMs }).reason, 'statement hash differs from the current text');
});

test('malformed messages are invalid, never thrown', () => {
  const v = V.vectors[0];
  const bad = [
    { ...v.message, account: '0x123' }, { ...v.message, residenceCountry: 'XX' }, { ...v.message, residenceCountry: 'br' },
    { ...v.message, issuedAt: 1794873600000 }, { ...v.message, issuedAt: '01' }, { ...v.message, expiresAt: '18446744073709551616' },
    { ...v.message, statementHash: v.message.statementHash.toUpperCase().replace('0X', '0x') }, { ...v.message, statementId: 'Has Space' },
  ];
  for (const m of bad) assert.equal(verifyAttestation({ message: m, signature: v.signature }, { statements, nowMs: v.nowMs }).status, 'invalid', JSON.stringify(m));
  for (const sig of [undefined, '0x', v.signature.slice(0, -2), v.signature + '00', 'zz']) {
    assert.equal(verifyAttestation({ message: v.message, signature: sig }, { statements, nowMs: v.nowMs }).status, 'invalid');
  }
  assert.throws(() => verifyAttestation(v, { statements }), /nowMs/);
});

test('typed data for the wallet carries every signed field', () => {
  const td = typedData(V.vectors[0].message);
  assert.deepEqual(td.domain, { name: 'PosternPerps Attestation', version: '1', chainId: 7007701 });
  assert.equal(td.primaryType, 'Attestation');
  assert.deepEqual(td.types.Attestation.map((f) => f.name), ['account', 'statementId', 'statementHash', 'residenceCountry', 'issuedAt', 'expiresAt']);
  assert.notEqual(td.domain.name, 'PosternPerps', 'never the engine action domain');
});

test('verified attestation feeds the decision; the record keeps no clear IP', () => {
  const v = V.vectors[0];
  const att = verifyAttestation(v, { statements, nowMs: v.nowMs });
  const list = fixtureList();
  const req = { mode: 'live', surface: 'api', kind: 'action', action: { type: 'PlaceOrder', reduceOnly: false },
    geo: { country: 'PA', subdivision: null, trusted: true }, signals: { tor: false, vpnProxy: false, hosting: true },
    addresses: [{ role: 'account', address: v.message.account }], sanctions: fixtureSanctions(), nowMs: Date.parse('2026-11-17T01:00:00Z'), attestation: att };
  assert.equal(decide(list, req).code, 'OK');
  const br = V.vectors.find((x) => x.name === 'second account valid');
  const att2 = verifyAttestation(br, { statements, nowMs: br.nowMs });
  assert.equal(att2.residenceCountry, 'BR');
  assert.equal(decide(list, { ...req, attestation: att2, addresses: [{ role: 'account', address: br.message.account }] }).code, 'GEO_RESTRICTED');
  const rec = attestationRecord({ message: v.message, signature: v.signature, verification: att, verifiedAtMs: v.nowMs, list, geo: req.geo, signals: req.signals, ipHmac: 'ab'.repeat(32), ipKeyId: '2026-11' });
  assert.equal(rec.schema, 'postern-perps/attestation-record');
  assert.equal(rec.digest, v.digest);
  assert.equal(rec.context.listSha256, list.sha256);
  assert.equal(JSON.stringify(rec).includes('ipAddress'), false);
});
