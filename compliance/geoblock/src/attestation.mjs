// User eligibility attestation: an EIP-712 signature by the account's own key over the exact statement text the
// user was shown, the residence country the user declares, and a validity period. UNAUDITED prototype. Not legal
// advice: the statement text, whether a signature is required, its validity and what it proves are for counsel
// (the text itself is agent 392's, compliance/legal/).
//
// Domain (coordinator instruction, 2026-11-17, after 391's report that wallets refuse a chainId other than the
// active chain): the engine's own SPEC 0.1.1 section 2.1 domain, EIP712Domain(name "PosternPerps", version "1",
// chainId 700770, verifyingContract = the PerpsVault address). It replaces this module's first domain
// ("PosternPerps Attestation", chain 7007701, no verifyingContract), which no real wallet would sign on 700770.
// Separation from engine actions now rests on the primary type: hashStruct starts with the type hash of
// "Attestation(...)", which no engine action type shares (388 asked to reserve the name), so a valid
// attestation signature is never a valid action signature and vice versa; verifyingContract binds it to one
// deployment. With no vault configured every function here refuses (throws): nothing is signed or accepted
// under a domain without a verifyingContract.

import { keccak256, utf8, concatBytes, hexToBytes, bytesToHex, normalizeEvmAddress } from './keccak.mjs';
import { recoverAddress } from './secp256k1.mjs';
import { RESERVED_CODES } from './list.mjs';

export const DOMAIN_BASE = Object.freeze({ name: 'PosternPerps', version: '1', chainId: 700770 });
export const DOMAIN_TYPE = 'EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)';
export const ATTESTATION_TYPE = 'Attestation(address account,string statementId,bytes32 statementHash,string residenceCountry,uint64 issuedAt,uint64 expiresAt)';
export const MAX_VALIDITY_MS = 400 * 24 * 3600 * 1000; // a yearly renewal with a month of grace
export const MAX_CLOCK_SKEW_MS = 5 * 60 * 1000;
const U64_MAX = (1n << 64n) - 1n;

const word = (v) => { const o = new Uint8Array(32); let x = BigInt(v); for (let i = 31; i >= 0; i--) { o[i] = Number(x & 0xffn); x >>= 8n; } return o; };

/** The vault address as lowercase hex; throws when absent, malformed or zero (refuse to sign or verify). */
export function requireVault(vault) {
  const v = normalizeEvmAddress(vault);
  if (v === null || /^0x0{40}$/.test(v)) throw new TypeError('attestation: no vault configured (verifyingContract required)');
  return v;
}

export function domain(vault) { return { ...DOMAIN_BASE, verifyingContract: requireVault(vault) }; }

export function domainSeparator(vault) {
  const v = requireVault(vault);
  return keccak256(concatBytes(keccak256(utf8(DOMAIN_TYPE)), keccak256(utf8(DOMAIN_BASE.name)), keccak256(utf8(DOMAIN_BASE.version)), word(DOMAIN_BASE.chainId), word(BigInt(v))));
}

/** keccak256 of the statement text exactly as displayed (UTF-8, no normalisation, no trimming). */
export function statementHash(text) { return bytesToHex(keccak256(utf8(text))); }

function checkMessage(m) {
  if (!m || typeof m !== 'object') return 'message missing';
  if (normalizeEvmAddress(m.account) === null) return 'account';
  if (typeof m.statementId !== 'string' || !/^[a-z0-9][a-z0-9./-]{0,63}$/.test(m.statementId)) return 'statementId';
  if (typeof m.statementHash !== 'string' || !/^0x[0-9a-f]{64}$/.test(m.statementHash)) return 'statementHash';
  if (typeof m.residenceCountry !== 'string' || !/^[A-Z]{2}$/.test(m.residenceCountry) || RESERVED_CODES.includes(m.residenceCountry)) return 'residenceCountry';
  for (const k of ['issuedAt', 'expiresAt']) {
    if (typeof m[k] !== 'string' || !/^(0|[1-9][0-9]{0,19})$/.test(m[k]) || BigInt(m[k]) > U64_MAX) return k;
  }
  return null;
}

/** EIP-712 digest of an attestation message (uint64 fields as decimal strings, the API's convention). */
export function attestationDigest(m, { vault } = {}) {
  const sep = domainSeparator(vault);
  const bad = checkMessage(m);
  if (bad) throw new TypeError(`attestation message: bad ${bad}`);
  const structHash = keccak256(concatBytes(
    keccak256(utf8(ATTESTATION_TYPE)),
    word(BigInt(normalizeEvmAddress(m.account))),
    keccak256(utf8(m.statementId)),
    hexToBytes(m.statementHash),
    keccak256(utf8(m.residenceCountry)),
    word(m.issuedAt),
    word(m.expiresAt),
  ));
  return keccak256(concatBytes(new Uint8Array([0x19, 0x01]), sep, structHash));
}

/** The typed data a frontend asks the wallet to sign (eth_signTypedData_v4 shape). */
export function typedData(m, { vault } = {}) {
  const bad = checkMessage(m);
  if (bad) throw new TypeError(`attestation message: bad ${bad}`);
  return {
    domain: domain(vault),
    primaryType: 'Attestation',
    types: {
      EIP712Domain: [{ name: 'name', type: 'string' }, { name: 'version', type: 'string' }, { name: 'chainId', type: 'uint256' }, { name: 'verifyingContract', type: 'address' }],
      Attestation: [
        { name: 'account', type: 'address' }, { name: 'statementId', type: 'string' }, { name: 'statementHash', type: 'bytes32' },
        { name: 'residenceCountry', type: 'string' }, { name: 'issuedAt', type: 'uint64' }, { name: 'expiresAt', type: 'uint64' },
      ],
    },
    message: { ...m },
  };
}

/**
 * verifyAttestation(record, {statements, nowMs}) -> {status: 'valid'|'invalid'|'expired', reason, residenceCountry, signer}
 * `record` = {message, signature: 0x + 130 hex}. `statements` = {statementId: statementHash} of the texts the
 * operator currently accepts (a text revision retires older ids: their records become 'invalid').
 * Verification is in process (never a subprocess exit code), with the strict signature rules of secp256k1.mjs.
 */
export function verifyAttestation(record, { statements, nowMs, vault }) {
  const no = (status, reason) => ({ status, reason, residenceCountry: null, signer: null });
  if (typeof nowMs !== 'number' || !Number.isFinite(nowMs)) throw new TypeError('nowMs required');
  requireVault(vault);
  const m = record?.message;
  const bad = checkMessage(m);
  if (bad) return no('invalid', `message.${bad}`);
  if (!Object.hasOwn(statements ?? {}, m.statementId)) return no('invalid', 'statement not current');
  if (statements[m.statementId] !== m.statementHash) return no('invalid', 'statement hash differs from the current text');
  if (typeof record.signature !== 'string' || !/^0x[0-9a-fA-F]{130}$/.test(record.signature)) return no('invalid', 'signature shape');
  const signer = recoverAddress(attestationDigest(m, { vault }), hexToBytes(record.signature));
  if (signer === null) return no('invalid', 'signature does not recover (length, v, r/s range or high-s)');
  if (signer !== normalizeEvmAddress(m.account)) return no('invalid', 'signer is not the account');
  const issued = Number(m.issuedAt); const expires = Number(m.expiresAt);
  if (!(expires > issued) || expires - issued > MAX_VALIDITY_MS) return no('invalid', 'validity period');
  if (issued > nowMs + MAX_CLOCK_SKEW_MS) return no('invalid', 'issued in the future');
  if (nowMs > expires) return { status: 'expired', reason: 'expired', residenceCountry: m.residenceCountry, signer };
  return { status: 'valid', reason: null, residenceCountry: m.residenceCountry, signer };
}

export const RECORD_SCHEMA = 'postern-perps/attestation-record';

/**
 * The record the operator keeps (see DESIGN.md "Logging and retention"): the signed message and signature (so
 * anyone can re-verify it), the verification result, and the access context at the time, with the IP address
 * only as a keyed hash (ipHmac under a rotating key id) and never in clear.
 */
export function attestationRecord({ message, signature, verification, verifiedAtMs, list, geo, signals, ipHmac, ipKeyId, vault }) {
  return {
    schema: RECORD_SCHEMA,
    version: 2,
    typedData: typedData(message, { vault }),
    signature,
    digest: bytesToHex(attestationDigest(message, { vault })),
    signer: verification.signer,
    status: verification.status,
    reason: verification.reason,
    verifiedAt: new Date(verifiedAtMs).toISOString(),
    context: {
      listVersion: list?.version ?? null,
      listSha256: list?.sha256 ?? null,
      country: geo?.country ?? null,
      subdivision: geo?.subdivision ?? null,
      signals: { tor: signals?.tor ?? null, vpnProxy: signals?.vpnProxy ?? null, hosting: signals?.hosting ?? null },
      ipHmac: ipHmac ?? null,
      ipKeyId: ipKeyId ?? null,
    },
  };
}

/**
 * Accepted statements from 392's compliance/legal/notices.json: {statementId: statementHash}. Refuses (throws)
 * when the file's statementHash is not keccak256 of its own text, so a hash that does not match the displayed
 * text is never accepted.
 */
export function statementsFromNotices(notices) {
  const a = notices?.attestation;
  if (!a || typeof a.statementId !== 'string' || typeof a.text !== 'string' || typeof a.statementHash !== 'string') throw new TypeError('notices: attestation block missing');
  if (statementHash(a.text) !== a.statementHash) throw new TypeError('notices: statementHash is not keccak256 of the statement text');
  if (!/^[a-z0-9][a-z0-9./-]{0,63}$/.test(a.statementId)) throw new TypeError('notices: statementId shape');
  return { [a.statementId]: a.statementHash };
}
