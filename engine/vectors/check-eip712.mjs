// Independent check of the engine's EIP-712 vectors (UNAUDITED prototype tooling).
// Usage: node check-eip712.mjs <eip712.json> <lib-dir>
// <lib-dir> holds an independent, dependency-free keccak / EIP-712 / secp256k1 implementation (agent 351's
// node/mock/lib, written separately from the Rust engine). The type strings below are typed from docs/SPEC.md 2.3,
// not taken from the engine's source.
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import path from 'node:path';

const [, , vecPath, libDir] = process.argv;
if (!vecPath || !libDir) { console.error('usage: node check-eip712.mjs <eip712.json> <lib-dir>'); process.exit(2); }
const lib = (f) => import(pathToFileURL(path.resolve(libDir, f)).href);
const { keccak256, concat, unhex, hex } = await lib('keccak.mjs');
const { hashStruct } = await lib('eip712.mjs');
const { recover } = await lib('secp256k1.mjs');

const T = {
  PlaceOrder: 'address account,uint64 nonce,uint32 marketId,bool isBuy,uint8 orderType,uint8 timeInForce,uint64 price,uint64 size,bool reduceOnly,uint64 triggerPrice,bytes32 clientOrderId',
  CancelOrder: 'address account,uint64 nonce,uint32 marketId,uint64 orderId',
  CancelAll: 'address account,uint64 nonce,uint32 marketId',
  UpdateLeverage: 'address account,uint64 nonce,uint32 marketId,uint32 leverage,bool isolated',
  UpdateMargin: 'address account,uint64 nonce,uint32 marketId,uint64 amount,bool add',
  Withdraw: 'address account,uint64 nonce,uint64 amount,address recipient',
  ApproveAgent: 'address account,uint64 nonce,address agent,string name,uint64 expiresAt',
  RevokeAgent: 'address account,uint64 nonce,address agent',
};
const fields = (s) => s.split(',').map((x) => { const [type, name] = x.split(' '); return { type, name }; });
const v = JSON.parse(readFileSync(vecPath, 'utf8'));
let checks = 0, fails = 0;
const eq = (what, a, b) => { checks++; if (a.toLowerCase() !== b.toLowerCase()) { fails++; console.error(`FAIL ${what}: ${a} != ${b}`); } };

const dom = hashStruct('EIP712Domain', fields('string name,string version,uint256 chainId,address verifyingContract'), v.domain);
eq('domainSeparator', hex(dom), v.domainSeparator);
for (const a of v.actions) {
  const sh = hashStruct(a.primaryType, fields(T[a.primaryType]), a.message);
  eq(`${a.primaryType}.structHash`, hex(sh), a.structHash);
  const d = keccak256(concat(Uint8Array.of(0x19, 0x01), dom, sh));
  eq(`${a.primaryType}.digest`, hex(d), a.digest);
  eq(`${a.primaryType}.signer`, recover(d, unhex(a.signature)), a.signer);
}
// Oracle observation: nested array per EIP-712 (keccak of the concatenated element hashes).
const o = v.oracle;
const pf = fields('uint32 marketId,uint64 price,uint8 sources');
const arr = concat(...o.prices.map((p) => hashStruct('OraclePrice', pf, p)));
const word = (x) => unhex('0x' + BigInt(x).toString(16).padStart(64, '0'));
const th = keccak256(new TextEncoder().encode('OracleObservation(uint64 round,uint64 timeMs,OraclePrice[] prices)OraclePrice(uint32 marketId,uint64 price,uint8 sources)'));
const osh = keccak256(concat(th, word(o.round), word(o.timeMs), keccak256(arr)));
eq('oracle.structHash', hex(osh), o.structHash);
const od = keccak256(concat(Uint8Array.of(0x19, 0x01), dom, osh));
eq('oracle.digest', hex(od), o.digest);
eq('oracle.signer', recover(od, unhex(o.signature)), o.signer);
console.log(`check-eip712: ${checks - fails}/${checks} checks passed`);
process.exit(fails ? 1 : 0);
