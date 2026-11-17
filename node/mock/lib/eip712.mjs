// Generic EIP-712 hashing for flat structs (address, uintN, intN, bool, bytes32, string). Dependency-free.
import { keccak256, concat, unhex } from './keccak.mjs';
const enc = new TextEncoder();
const word = (x) => {
  const v = BigInt.asUintN(256, BigInt(x));
  return new Uint8Array(Buffer.from(v.toString(16).padStart(64, '0'), 'hex'));
};
export function encodeType(name, fields) { return `${name}(${fields.map((f) => `${f.type} ${f.name}`).join(',')})`; }
function encodeField(type, value) {
  if (type === 'string') return keccak256(enc.encode(String(value)));
  if (type === 'bool') { if (typeof value !== 'boolean') throw new Error('bool expected'); return word(value ? 1 : 0); }
  if (type === 'address') {
    if (typeof value !== 'string' || !/^0x[0-9a-fA-F]{40}$/.test(value)) throw new Error('address expected');
    return word(BigInt(value));
  }
  if (type === 'bytes32') {
    if (typeof value !== 'string' || !/^0x[0-9a-fA-F]{64}$/.test(value)) throw new Error('bytes32 expected');
    return unhex(value);
  }
  const m = /^(u?)int(\d+)$/.exec(type);
  if (m) {
    if (!(typeof value === 'string' ? /^-?\d+$/.test(value) : Number.isSafeInteger(value))) throw new Error(`${type} expected`);
    const v = BigInt(value), bits = BigInt(m[2]);
    const lo = m[1] ? 0n : -(1n << (bits - 1n)), hi = m[1] ? (1n << bits) - 1n : (1n << (bits - 1n)) - 1n;
    if (v < lo || v > hi) throw new Error(`${type} out of range`);
    return word(v);
  }
  throw new Error(`unsupported type ${type}`);
}
export function hashStruct(name, fields, message) {
  const parts = [keccak256(enc.encode(encodeType(name, fields)))];
  for (const f of fields) {
    if (!(f.name in message)) throw new Error(`missing field ${f.name}`);
    try { parts.push(encodeField(f.type, message[f.name])); } catch (e) { throw new Error(`${f.name}: ${e.message}`); }
  }
  for (const k of Object.keys(message)) if (!fields.some((f) => f.name === k)) throw new Error(`unknown field ${k}`);
  return keccak256(concat(...parts));
}
const DOMAIN_FIELDS = [{ name: 'name', type: 'string' }, { name: 'version', type: 'string' }, { name: 'chainId', type: 'uint256' }];
export function domainSeparator(domain) { return hashStruct('EIP712Domain', DOMAIN_FIELDS, domain); }
export function typedDataHash(domain, name, fields, message) {
  return keccak256(concat(Uint8Array.of(0x19, 0x01), domainSeparator(domain), hashStruct(name, fields, message)));
}
