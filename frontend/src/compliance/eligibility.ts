/**
 * The eligibility attestation as an EIP-712 signature (owner of the format: agent 397,
 * compliance/geoblock/src/attestation.mjs at 70a9df0; statement text and ids: agent 392,
 * compliance/legal/notices.json NOTICES-0.2-20261117). UNAUDITED prototype; wording and whether a signature is
 * required are for counsel.
 *
 * PINNED here, like the action types: the page signs only this type and this statement, never typed data or text
 * received from a server. The statement hash is recomputed here from the text the page shows (keccak-256 of the
 * exact UTF-8 text) and must equal the pinned hash, so the page cannot show one text and sign another.
 */
import { keccak256, recoverTypedDataAddress, toBytes, type Hex } from 'viem';
import { signatureProblem, type Signer } from '../lib/sign';
import { EIP712_DOMAIN_TYPE, type Domain, type TypedData } from '../lib/eip712';

export const STATEMENT_ID = 'attest-0.1-20261117';
export const STATEMENT_DOCUMENT = 'ATTEST-0.1-20261117';
export const STATEMENT_TEXT =
  'ATTEST-0.1-20261117\n' +
  'I am not a resident of, located in, or incorporated in a restricted jurisdiction, and I will not use a VPN or similar tool to hide my location.\n' +
  'I am not a person or entity on a sanctions list, and I am not acting for one.\n' +
  'I have read the Risk Disclosure and the Terms of Use, and I understand that I can lose all the funds I deposit.';
/** notices.json `attestation.statementHash` (392, b91d429..47f95bd). */
export const STATEMENT_HASH = '0xcfdf20a5c360057590fa84786ff9e9b76180ebcbb55390ce7a68f37b3b1b6621';

/**
 * 397 (c964dcb): the attestation signs under the SPEC 0.1.1 action domain {PosternPerps, 1, 700770, vault}; the 7007701
 * domain is retired. Separation from actions rests on the primary type name `Attestation` (reserved in SPEC).
 * No vault, no domain: nothing is signed.
 */
export const ATTESTATION_TYPES = {
  Attestation: [
    { name: 'account', type: 'address' },
    { name: 'statementId', type: 'string' },
    { name: 'statementHash', type: 'bytes32' },
    { name: 'residenceCountry', type: 'string' },
    { name: 'issuedAt', type: 'uint64' },
    { name: 'expiresAt', type: 'uint64' },
  ],
} as const;
/** 397: validity at most 400 days. The page asks for one year. */
export const VALIDITY_MS = 365 * 86_400_000;
const RESERVED = ['XX', 'T1', 'ZZ', 'A1', 'A2', 'O1', 'EU', 'AP'];

export type AttestationMessage = { account: string; statementId: string; statementHash: string; residenceCountry: string; issuedAt: string; expiresAt: string };
export type AttestationRecord = { message: AttestationMessage; signature: string };

export function statementProblem(): string | null {
  return keccak256(toBytes(STATEMENT_TEXT)) === STATEMENT_HASH ? null : 'the statement shown differs from the pinned statement';
}

export function attestationMessage(account: string, residenceCountry: string, now: number): AttestationMessage {
  const country = residenceCountry.trim().toUpperCase();
  if (!/^[A-Z]{2}$/.test(country) || RESERVED.includes(country)) throw new Error('Choose your country of residence (two letters, ISO 3166).');
  return { account: account.toLowerCase(), statementId: STATEMENT_ID, statementHash: STATEMENT_HASH, residenceCountry: country, issuedAt: String(now), expiresAt: String(now + VALIDITY_MS) };
}

export function attestationTypedData(message: AttestationMessage, domain: Domain) {
  return {
    domain: { ...domain },
    types: { EIP712Domain: EIP712_DOMAIN_TYPE, ...ATTESTATION_TYPES },
    primaryType: 'Attestation' as const,
    message: { ...message },
  };
}

const forViem = (m: AttestationMessage, domain: Domain) =>
  ({ domain: { ...domain }, types: ATTESTATION_TYPES, primaryType: 'Attestation', message: { ...m, issuedAt: BigInt(m.issuedAt), expiresAt: BigInt(m.expiresAt) } }) as const;

/** Signs with the MAIN wallet only, checks the signature here, returns the record. */
export async function signAttestation(signer: Signer, message: AttestationMessage, domain: Domain | null): Promise<AttestationRecord> {
  if (!domain) throw new Error('The vault is not deployed: there is no signing domain, nothing is signed.');
  if (signer.kind !== 'wallet') throw new Error('The attestation is signed by your main wallet only.');
  if (signer.address !== message.account) throw new Error('The wallet account changed.');
  const shown = statementProblem();
  if (shown) throw new Error(`Refused: ${shown}.`);
  const signature = await signer.sign(attestationTypedData(message, domain) as unknown as TypedData);
  const form = signatureProblem(signature);
  if (form) throw new Error(`The wallet returned an unusable signature: ${form}.`);
  const recovered = (await recoverTypedDataAddress({ ...forViem(message, domain), signature: signature as Hex } as never)).toLowerCase();
  if (recovered !== message.account) throw new Error('The wallet signed with another account than the one shown.');
  return { message, signature };
}

const KEY = 'postern.perps.attestation.v2';
const session = new Map<string, AttestationRecord>();
const current = (r: AttestationRecord | undefined, now: number) =>
  r && r.message.statementId === STATEMENT_ID && r.message.statementHash === STATEMENT_HASH && Number(r.message.expiresAt) > now ? r : null;
/** The account's attestation for the CURRENT statement, unexpired; a new statement text asks again. */
export function storedAttestation(account: string, now: number = Date.now()): AttestationRecord | null {
  const a = account.toLowerCase();
  const inTab = current(session.get(a), now);
  if (inTab) return inTab;
  try {
    const list: unknown = JSON.parse(localStorage.getItem(KEY) ?? '[]');
    if (!Array.isArray(list)) return null;
    return current(list.find((x) => x?.message?.account === a) as AttestationRecord | undefined, now);
  } catch {
    return null;
  }
}
export function keepAttestation(record: AttestationRecord) {
  session.set(record.message.account, record);
  try {
    const list: unknown = JSON.parse(localStorage.getItem(KEY) ?? '[]');
    const rest = Array.isArray(list) ? list.filter((x) => x?.message?.account !== record.message.account) : [];
    localStorage.setItem(KEY, JSON.stringify([...rest, record]));
  } catch {
    /* this tab only */
  }
}
