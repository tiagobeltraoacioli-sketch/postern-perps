#!/usr/bin/env node
// Dev tool: generate vectors/attestation-vectors.json with an INDEPENDENT implementation (@noble/curves and
// @noble/hashes 1.8.0, MIT), so that src/attestation.mjs + src/secp256k1.mjs + src/keccak.mjs are checked against
// code they share nothing with. Keys are throwaway, derived from public strings (keccak256 of a label): never use
// them for anything else. Not needed at run time; the committed JSON is what the tests read.
// node tools/gen-attestation-vectors.mjs <path to a node_modules directory containing @noble/curves and @noble/hashes>

import { writeFileSync, readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const nm = process.argv[2];
if (!nm) { console.error('usage: gen-attestation-vectors.mjs <node_modules dir>'); process.exit(2); }
const { secp256k1 } = await import(pathToFileURL(`${nm}/@noble/curves/secp256k1.js`).href);
const { keccak_256 } = await import(pathToFileURL(`${nm}/@noble/hashes/sha3.js`).href);

const te = new TextEncoder();
const hex = (b) => '0x' + Buffer.from(b).toString('hex');
const k = (x) => keccak_256(typeof x === 'string' ? te.encode(x) : x);
const w = (v) => { const b = Buffer.alloc(32); let x = BigInt(v); for (let i = 31; i >= 0; i--) { b[i] = Number(x & 0xffn); x >>= 8n; } return b; };
const cat = (...a) => Buffer.concat(a.map((x) => Buffer.from(x)));

// SPEC 0.1.1 section 2.1 domain: PosternPerps / 1 / 700770 / verifyingContract = vault (a throwaway address
// derived from a public label for the vectors; never a deployed contract).
const DOMAIN_TYPE = 'EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)';
const TYPE = 'Attestation(address account,string statementId,bytes32 statementHash,string residenceCountry,uint64 issuedAt,uint64 expiresAt)';
const vault = hex(k('postern-perps-397-test-vault').slice(12));
const otherVault = hex(k('postern-perps-397-test-vault-2').slice(12));
const sepFor = (v) => k(cat(k(DOMAIN_TYPE), k('PosternPerps'), k('1'), w(700770), w(BigInt(v))));
const domainSep = sepFor(vault);
const oldSep = k(cat(k('EIP712Domain(string name,string version,uint256 chainId)'), k('PosternPerps Attestation'), k('1'), w(7007701)));
const digestWith = (sep, m) => k(cat(Buffer.from([0x19, 0x01]), sep, k(cat(k(TYPE), w(BigInt(m.account)), k(m.statementId), Buffer.from(m.statementHash.slice(2), 'hex'), k(m.residenceCountry), w(m.issuedAt), w(m.expiresAt)))));
const digest = (m) => k(cat(Buffer.from([0x19, 0x01]), domainSep, k(cat(k(TYPE), w(BigInt(m.account)), k(m.statementId), Buffer.from(m.statementHash.slice(2), 'hex'), k(m.residenceCountry), w(m.issuedAt), w(m.expiresAt)))));
const addrOf = (priv) => hex(k(secp256k1.getPublicKey(priv, false).slice(1)).slice(12));
const N = secp256k1.CURVE.n;

// 392's statement (compliance/legal/notices.json): id and text as published; the hash is recomputed here.
const notices = JSON.parse(readFileSync(new URL('../../legal/notices.json', import.meta.url), 'utf8'));
const statementText = notices.attestation.text;
const statementId = notices.attestation.statementId;
const statementHash = hex(k(statementText));
if (statementHash !== notices.attestation.statementHash) throw new Error('notices.json statementHash is not keccak256 of its text');
const priv1 = k('postern-perps-397-throwaway-attestation-key-1');
const priv2 = k('postern-perps-397-throwaway-attestation-key-2');
const acct1 = addrOf(priv1); const acct2 = addrOf(priv2);

function sign(priv, m, dg = digest(m)) {
  const sig = secp256k1.sign(dg, priv, { lowS: true });
  return { r: sig.r, s: sig.s, v: 27 + sig.recovery };
}
const enc = ({ r, s, v }) => hex(cat(w(r), w(s), Buffer.from([v])));

const base = { account: acct1, statementId, statementHash, residenceCountry: 'PA', issuedAt: '1794873600000', expiresAt: '1826409600000' };
const s1 = sign(priv1, base);
const now = 1794880000000;
const vectors = [
  { name: 'valid', message: base, signature: enc(s1), nowMs: now, expect: { status: 'valid', signer: acct1 } },
  { name: 'high-s twin of a valid signature is refused (EIP-2)', message: base, signature: enc({ r: s1.r, s: N - s1.s, v: s1.v === 27 ? 28 : 27 }), nowMs: now, expect: { status: 'invalid' } },
  { name: 'v = 0/1 form is refused (only 27/28)', message: base, signature: enc({ ...s1, v: s1.v - 27 }), nowMs: now, expect: { status: 'invalid' } },
  { name: 'flipped v recovers another key: signer is not the account', message: base, signature: enc({ ...s1, v: s1.v === 27 ? 28 : 27 }), nowMs: now, expect: { status: 'invalid' } },
  { name: 'signed by another key for this account', message: base, signature: enc(sign(priv2, base)), nowMs: now, expect: { status: 'invalid' } },
  { name: 'residence changed after signing', message: { ...base, residenceCountry: 'CH' }, signature: enc(s1), nowMs: now, expect: { status: 'invalid' } },
  { name: 'expired', message: base, signature: enc(s1), nowMs: 1826409600001, expect: { status: 'expired', signer: acct1 } },
  { name: 'issued in the future beyond skew', message: base, signature: enc(s1), nowMs: 1794873600000 - 5 * 60 * 1000 - 1, expect: { status: 'invalid' } },
  { name: 'validity longer than 400 days', message: { ...base, account: acct2, expiresAt: String(1794873600000 + 401 * 86400000) }, signature: null, sign: 2, nowMs: now, expect: { status: 'invalid' } },
  { name: 'signed for another vault (another deployment) is refused', message: base, signature: enc(sign(priv1, base, digestWith(sepFor(otherVault), base))), nowMs: now, expect: { status: 'invalid' } },
  { name: 'signed under the retired 7007701 domain is refused', message: base, signature: enc(sign(priv1, base, digestWith(oldSep, base))), nowMs: now, expect: { status: 'invalid' } },
  { name: 'r = 0 refused', message: base, signature: enc({ r: 0n, s: s1.s, v: s1.v }), nowMs: now, expect: { status: 'invalid' } },
  { name: 'second account valid', message: { ...base, account: acct2, residenceCountry: 'BR' }, signature: null, sign: 2, nowMs: now, expect: { status: 'valid', signer: acct2 } },
];
for (const v of vectors) {
  if (v.signature === null) { v.signature = enc(sign(v.sign === 2 ? priv2 : priv1, v.message)); delete v.sign; }
  v.digest = hex(digest(v.message));
}
writeFileSync(new URL('../vectors/attestation-vectors.json', import.meta.url), JSON.stringify({
  generator: 'tools/gen-attestation-vectors.mjs with @noble/curves 1.8.0 and @noble/hashes 1.8.0 (independent implementation)',
  domain: { name: 'PosternPerps', version: '1', chainId: 700770, verifyingContract: vault },
  vault,
  domainSeparator: hex(domainSep),
  typeHash: hex(k(TYPE)),
  statement: { id: statementId, text: statementText, hash: statementHash },
  accounts: [acct1, acct2],
  keccakVectors: [0, 1, 135, 136, 137, 271, 272, 300, 1000].map((n) => { const b = Buffer.alloc(n); for (let i = 0; i < n; i++) b[i] = (i * 7 + 3) & 0xff; return { input: hex(b), keccak256: hex(k(b)) }; }),
  vectors,
}, null, 1) + '\n');
console.log(`wrote ${vectors.length} vectors`);
