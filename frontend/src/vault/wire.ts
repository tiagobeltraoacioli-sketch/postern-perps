/**
 * Reads the proofs the exchange API (or a pasted file) hands over into the exact values the vault takes.
 * UNAUDITED prototype.
 *
 * Nothing here is trusted because it parsed: vault.ts hashes the result and compares it with the roots the vault
 * holds. Parsing is strict so that a value can never be read two ways: integers are canonical decimal strings (or
 * safe JSON integers) in base units, never a decimal fraction; addresses are 20 bytes; hashes 32 bytes; a proof is a
 * list of exactly 32 hashes. Anything else is refused with the field's name.
 *
 * Shapes accepted (docs/API.md 4 and 4.1; field names as the engine's SPEC 11 leaf data):
 *   withdrawal certificate: { record: { withdrawalId, account, recipient, amount, height }, proof: [32 hashes] }
 *                           (`proof` may also be { hashes: [...] } or named `siblings`)
 *   account proof:          { leafData: { account, index, balance, escapeValue, positionsHash }, proof: [32 hashes] }
 *                           (`leafData` may be named `leaf` when it is an object; `account` may be named `address`;
 *                            `index` may sit beside `leafData`)
 */
import type { Hex } from 'viem';
import { TREE_DEPTH, type AccountLeaf, type WithdrawalRecord } from './merkle';

export class WireError extends Error {}
const fail = (what: string): never => {
  throw new WireError(what);
};
const object = (v: unknown, what: string): Record<string, unknown> => (v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : fail(`${what} is missing`));

function integer(v: unknown, what: string, signed = false): bigint {
  if (typeof v === 'number') {
    if (!Number.isSafeInteger(v)) fail(`${what} is not an integer`);
    return BigInt(v);
  }
  if (typeof v !== 'string' || !(signed ? /^-?(0|[1-9]\d{0,40})$/ : /^(0|[1-9]\d{0,40})$/).test(v) || v === '-0') fail(`${what} is not an integer in base units`);
  return BigInt(v as string);
}
function address(v: unknown, what: string): Hex {
  if (typeof v !== 'string' || !/^0x[0-9a-fA-F]{40}$/.test(v)) fail(`${what} is not an address`);
  return (v as string).toLowerCase() as Hex;
}
function hash(v: unknown, what: string): Hex {
  if (typeof v !== 'string' || !/^0x[0-9a-fA-F]{64}$/.test(v)) fail(`${what} is not a 32-byte hash`);
  return (v as string).toLowerCase() as Hex;
}
function siblings(v: unknown): Hex[] {
  const list = Array.isArray(v) ? v : v && typeof v === 'object' && Array.isArray((v as { hashes?: unknown }).hashes) ? (v as { hashes: unknown[] }).hashes : fail('the proof is missing');
  if (list.length !== TREE_DEPTH) fail(`the proof has ${list.length} hashes; the vault takes exactly ${TREE_DEPTH}`);
  return list.map((h, i) => hash(h, `proof hash ${i}`));
}
const MAX_U64 = (1n << 64n) - 1n;
const u64 = (v: bigint, what: string) => (v > MAX_U64 ? fail(`${what} is out of range`) : v);

export function parseWithdrawalCertificate(certificate: unknown): { record: WithdrawalRecord; siblings: Hex[] } {
  const c = object(certificate, 'the certificate');
  const r = object(c.record, 'the certificate record');
  const record: WithdrawalRecord = {
    withdrawalId: u64(integer(r.withdrawalId ?? r.index, 'withdrawalId'), 'withdrawalId'),
    account: address(r.account, 'account'),
    recipient: address(r.recipient, 'recipient'),
    amount: u64(integer(r.amount, 'amount'), 'amount'),
    height: u64(integer(r.height, 'height'), 'height'),
  };
  return { record, siblings: siblings(c.siblings ?? c.proof) };
}

export function parseAccountProof(proof: unknown): { leaf: AccountLeaf; siblings: Hex[] } {
  const p = object(proof, 'the proof');
  const data = object(typeof p.leaf === 'object' && p.leaf !== null ? p.leaf : p.leafData, 'the account leaf');
  const index = integer(data.index ?? p.index, 'index');
  if (index > 0xffffffffn) fail('index is out of range');
  const balance = integer(data.balance, 'balance', true);
  if (balance < -(1n << 127n) || balance >= 1n << 127n) fail('balance is out of range');
  const leaf: AccountLeaf = {
    account: address(data.account ?? data.address, 'account'),
    index: Number(index),
    balance,
    escapeValue: u64(integer(data.escapeValue, 'escapeValue'), 'escapeValue'),
    positionsHash: hash(data.positionsHash, 'positionsHash'),
  };
  return { leaf, siblings: siblings(p.siblings ?? p.proof) };
}

/** A pasted proof: JSON text of at most 64 KiB. */
export function parsePastedProof(text: string): { leaf: AccountLeaf; siblings: Hex[] } {
  if (text.length > 65_536) fail('the text is too long to be a proof');
  let json: unknown;
  try {
    json = JSON.parse(text);
  } catch {
    fail('the text is not JSON');
  }
  return parseAccountProof(json);
}
