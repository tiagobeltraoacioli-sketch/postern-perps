import test from 'node:test';
import assert from 'node:assert/strict';
import { verifyAttestation, attestationDigest, domainSeparator, statementHash, typedData, attestationRecord, ATTESTATION_TYPE, statementsFromNotices } from '../src/attestation.mjs';
import { keccak256, utf8, bytesToHex } from '../src/keccak.mjs';
import { decide } from '../src/decide.mjs';
import { readJson, fixtureList, fixtureSanctions } from './helpers.mjs';

const V = readJson('vectors/attestation-vectors.json');
const vault = V.vault;
// the accepted statements come from 392's notices.json (hash checked against its own text)
const statements = statementsFromNotices(readJson('../legal/notices.json'));

test('domain separator, type hash and statement hash equal the independent implementation', () => {
  assert.equal(bytesToHex(domainSeparator(vault)), V.domainSeparator);
  assert.deepEqual(statements, { [V.statement.id]: V.statement.hash });
  assert.equal(V.statement.id, 'attest-0.1-20261117');
  assert.equal(bytesToHex(keccak256(utf8(ATTESTATION_TYPE))), V.typeHash);
  assert.equal(statementHash(V.statement.text), V.statement.hash);
});

test('attestation vectors (independent signer)', () => {
  assert.equal(V.vectors.length, 13);
  for (const v of V.vectors) {
    assert.equal(bytesToHex(attestationDigest(v.message, { vault })), v.digest, `${v.name}: digest`);
    const r = verifyAttestation({ message: v.message, signature: v.signature }, { statements, nowMs: v.nowMs, vault });
    assert.equal(r.status, v.expect.status, `${v.name}: ${r.reason}`);
    if (v.expect.signer) assert.equal(r.signer, v.expect.signer, v.name);
  }
});

test('a statement that is no longer current, or whose text changed, is invalid', () => {
  const v = V.vectors[0];
  assert.equal(verifyAttestation(v, { statements: {}, nowMs: v.nowMs, vault }).reason, 'statement not current');
  assert.equal(verifyAttestation(v, { statements: { [V.statement.id]: statementHash(V.statement.text + ' ') }, nowMs: v.nowMs, vault }).reason, 'statement hash differs from the current text');
  const bad = readJson('../legal/notices.json'); bad.attestation.text += ' ';
  assert.throws(() => statementsFromNotices(bad), /not keccak256 of the statement text/);
});

test('malformed messages are invalid, never thrown', () => {
  const v = V.vectors[0];
  const bad = [
    { ...v.message, account: '0x123' }, { ...v.message, residenceCountry: 'XX' }, { ...v.message, residenceCountry: 'br' },
    { ...v.message, issuedAt: 1794873600000 }, { ...v.message, issuedAt: '01' }, { ...v.message, expiresAt: '18446744073709551616' },
    { ...v.message, statementHash: v.message.statementHash.toUpperCase().replace('0X', '0x') }, { ...v.message, statementId: 'Has Space' },
  ];
  for (const m of bad) assert.equal(verifyAttestation({ message: m, signature: v.signature }, { statements, nowMs: v.nowMs, vault }).status, 'invalid', JSON.stringify(m));
  for (const sig of [undefined, '0x', v.signature.slice(0, -2), v.signature + '00', 'zz']) {
    assert.equal(verifyAttestation({ message: v.message, signature: sig }, { statements, nowMs: v.nowMs, vault }).status, 'invalid');
  }
  assert.throws(() => verifyAttestation(v, { statements, vault }), /nowMs/);
});

test('typed data for the wallet: the SPEC 0.1.1 domain on the active chain 700770, bound to the vault', () => {
  const td = typedData(V.vectors[0].message, { vault });
  assert.deepEqual(td.domain, { name: 'PosternPerps', version: '1', chainId: 700770, verifyingContract: vault });
  assert.deepEqual(td.types.EIP712Domain.map((f) => f.name), ['name', 'version', 'chainId', 'verifyingContract']);
  assert.equal(td.primaryType, 'Attestation');
  assert.deepEqual(td.types.Attestation.map((f) => f.name), ['account', 'statementId', 'statementHash', 'residenceCountry', 'issuedAt', 'expiresAt']);
});

test('no vault configured: refuse to build typed data, a digest, or to verify', () => {
  const v = V.vectors[0];
  for (const bad of [undefined, null, '', '0x', '0x0000000000000000000000000000000000000000', '0x646a8aadabe753a101df5f1c1f48eeebbd30415', 'vault']) {
    assert.throws(() => typedData(v.message, { vault: bad }), /no vault configured/, String(bad));
    assert.throws(() => attestationDigest(v.message, { vault: bad }), /no vault configured/, String(bad));
    assert.throws(() => verifyAttestation(v, { statements, nowMs: v.nowMs, vault: bad }), /no vault configured/, String(bad));
  }
  assert.throws(() => typedData(v.message), /no vault configured/);
});

test('verified attestation feeds the decision; the record keeps no clear IP', () => {
  const v = V.vectors[0];
  const att = verifyAttestation(v, { statements, nowMs: v.nowMs, vault });
  const list = fixtureList();
  const req = { mode: 'live', surface: 'api', kind: 'action', action: { type: 'PlaceOrder', reduceOnly: false },
    geo: { country: 'PA', subdivision: null, trusted: true }, signals: { tor: false, vpnProxy: false, hosting: true },
    addresses: [{ role: 'account', address: v.message.account }], sanctions: fixtureSanctions(), nowMs: Date.parse('2026-11-17T01:00:00Z'), attestation: att };
  assert.equal(decide(list, req).code, 'OK');
  const br = V.vectors.find((x) => x.name === 'second account valid');
  const att2 = verifyAttestation(br, { statements, nowMs: br.nowMs, vault });
  assert.equal(att2.residenceCountry, 'BR');
  assert.equal(decide(list, { ...req, attestation: att2, addresses: [{ role: 'account', address: br.message.account }] }).code, 'GEO_RESTRICTED');
  const rec = attestationRecord({ message: v.message, signature: v.signature, verification: att, verifiedAtMs: v.nowMs, list, geo: req.geo, signals: req.signals, ipHmac: 'ab'.repeat(32), ipKeyId: '2026-11', vault });
  assert.equal(rec.schema, 'postern-perps/attestation-record');
  assert.equal(rec.digest, v.digest);
  assert.equal(rec.context.listSha256, list.sha256);
  assert.equal(JSON.stringify(rec).includes('ipAddress'), false);
});
