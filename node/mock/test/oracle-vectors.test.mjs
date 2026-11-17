// 394's oracle EIP-712 vectors (node/oracle/vectors/eip712.json) checked with the mock's independent JS libraries:
// digest, hashStruct and signer of every observation; the mock's oracle types equal the vectors'. UNAUDITED prototype.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { hashStruct, typedDataHash, domainSeparator, encodeType } from '../lib/eip712.mjs';
import { keccak256, hex, unhex } from '../lib/keccak.mjs';
import { recover } from '../lib/secp256k1.mjs';
import { ORACLE_TYPES } from '../lib/encoding.mjs';

// Integers above 2^53 appear as JSON numbers in the vectors ("max-values"); read them as exact decimal strings.
const V = JSON.parse(readFileSync(new URL('../../oracle/vectors/eip712.json', import.meta.url), 'utf8').replace(/([:\[,]\s*)(\d{16,})(?=\s*[,\]}])/g, '$1"$2"'));

test('394 oracle vectors: type hashes, domain separator, digests and signers reproduce independently', () => {
  const enc = new TextEncoder();
  assert.equal(hex(keccak256(enc.encode(encodeType('OracleObservation', ORACLE_TYPES.OracleObservation, ORACLE_TYPES)))), V.typeHashes.OracleObservation);
  assert.equal(hex(keccak256(enc.encode(encodeType('OraclePrice', ORACLE_TYPES.OraclePrice, ORACLE_TYPES)))), V.typeHashes.OraclePrice);
  const domain = { name: 'PosternPerps', version: '1', chainId: 700770, verifyingContract: V.verifyingContract };
  assert.equal(hex(domainSeparator(domain)), V.domainSeparator);
  assert.ok(V.observations.length >= 2);
  for (const o of V.observations) {
    const td = typeof o.typedData === 'string' ? JSON.parse(o.typedData) : o.typedData;
    const msg = td.message;
    assert.equal(hex(hashStruct('OracleObservation', ORACLE_TYPES.OracleObservation, msg, ORACLE_TYPES)), o.hashStruct, o.name);
    const d = typedDataHash(domain, 'OracleObservation', ORACLE_TYPES.OracleObservation, msg, ORACLE_TYPES);
    assert.equal(hex(d), o.digest, o.name);
    assert.equal(recover(d, unhex(o.signature)), o.signer.toLowerCase(), o.name);
  }
});
