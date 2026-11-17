// Canonical block-input encoding of docs/SPEC.md v0.1 section 3.2 (and 394's OracleUpdate bytes, node/oracle
// README section 4), with the JSON form of S-3.2.2 (integers as decimal strings, bytes as 0x hex). Encoder and a
// strict decoder (no trailing bytes, tag order, bounds), so the mock, the node and any third party can check that
// `inputsEncoded` and `inputs` say the same thing. UNAUDITED prototype.
import { unhex, hex } from './keccak.mjs';

export const TAG = { oracle: 1, deposit: 2, forced: 3, action: 4 };
const KIND = { 1: 'oracle', 2: 'deposit', 3: 'forced', 4: 'action' };
export const TYPE_ID = { PlaceOrder: 1, CancelOrder: 2, CancelAll: 3, UpdateLeverage: 4, UpdateMargin: 5, Withdraw: 6, ApproveAgent: 7, RevokeAgent: 8 };
const TYPE_NAME = Object.fromEntries(Object.entries(TYPE_ID).map(([k, v]) => [v, k]));
const WIDTH = { address: 20, uint64: 8, uint32: 4, uint8: 1, bool: 1, bytes32: 32 };

class W {
  constructor() { this.parts = []; this.n = 0; }
  bytes(b) { this.parts.push(b); this.n += b.length; }
  uint(v, width) {
    const x = BigInt(v);
    if (x < 0n || x >= 1n << BigInt(width * 8)) throw new Error(`value out of range for ${width} bytes`);
    const b = new Uint8Array(width);
    let y = x;
    for (let i = width - 1; i >= 0; i--) { b[i] = Number(y & 255n); y >>= 8n; }
    this.bytes(b);
  }
  out() {
    const o = new Uint8Array(this.n);
    let k = 0;
    for (const p of this.parts) { o.set(p, k); k += p.length; }
    return o;
  }
}
class R {
  constructor(b) { this.b = b; this.i = 0; }
  take(n) {
    if (this.i + n > this.b.length) throw new Error('truncated input');
    const s = this.b.subarray(this.i, this.i + n);
    this.i += n;
    return s;
  }
  uint(n) { let v = 0n; for (const x of this.take(n)) v = (v << 8n) | BigInt(x); return v; }
  done() { if (this.i !== this.b.length) throw new Error('trailing bytes'); }
}

function field(w, type, v) {
  if (type === 'address') return w.bytes(unhex(v.toLowerCase()));
  if (type === 'bytes32') return w.bytes(unhex(v.toLowerCase()));
  if (type === 'bool') return w.uint(v ? 1 : 0, 1);
  if (type === 'string') {
    const s = new TextEncoder().encode(v);
    w.uint(s.length, 2);
    return w.bytes(s);
  }
  if (WIDTH[type]) return w.uint(v, WIDTH[type]);
  throw new Error(`no SPEC 3.2 encoding for ${type}`);
}
function readField(r, type) {
  if (type === 'address' || type === 'bytes32') return hex(r.take(WIDTH[type]));
  if (type === 'bool') {
    const b = Number(r.uint(1));
    if (b > 1) throw new Error('bool must be 0 or 1');
    return b === 1;
  }
  if (type === 'string') {
    const n = Number(r.uint(2));
    return new TextDecoder('utf-8', { fatal: true }).decode(r.take(n));
  }
  if (WIDTH[type]) return r.uint(WIDTH[type]).toString();
  throw new Error(`no SPEC 3.2 encoding for ${type}`);
}

function payload(input, eip712) {
  const w = new W();
  if (input.kind === 'action') {
    const id = TYPE_ID[input.type];
    if (!id) throw new Error(`unknown action type ${input.type}`);
    w.uint(id, 1);
    for (const f of eip712.types[input.type]) field(w, f.type, input.message[f.name]);
    w.bytes(unhex(input.signature));
  } else if (input.kind === 'deposit') {
    const p = input.payload;
    w.uint(p.depositIndex, 8);
    field(w, 'address', p.account);
    w.uint(p.amount, 8);
  } else if (input.kind === 'forced') {
    const p = input.payload;
    w.uint(p.forcedIndex, 8);
    field(w, 'address', p.account);
    w.uint(p.kind, 1);
    w.uint(p.deadline, 8);
    w.uint(p.arg, 8);
  } else if (input.kind === 'oracle') {
    const p = input.payload;
    w.uint(p.round, 8);
    w.uint(p.observations.length, 1);
    for (const o of p.observations) {
      w.uint(o.signer, 1);
      w.uint(o.timeMs, 8);
      w.uint(o.prices.length, 2);
      for (const x of o.prices) { w.uint(x.marketId, 4); w.uint(x.price, 8); w.uint(x.sources, 1); }
      w.bytes(unhex(o.signature));
    }
  } else throw new Error(`unknown input kind ${input.kind}`);
  return w.out();
}

/** inputsEncoded = be32(count) || (tag:u8 || be32(len) || payload)*, inputs in tag order (SPEC 3.2). */
export function encodeInputs(inputs, eip712) {
  const w = new W();
  w.uint(inputs.length, 4);
  let last = 0;
  for (const inp of inputs) {
    const tag = TAG[inp.kind];
    if (tag < last) throw new Error('BLOCK_ORDER: inputs not in tag order');
    if (tag === 1 && last === 1) throw new Error('BLOCK_ORDER: at most one oracle input');
    last = tag;
    const p = payload(inp, eip712);
    w.uint(tag, 1);
    w.uint(p.length, 4);
    w.bytes(p);
  }
  return w.out();
}

/** Strict decoder: the JSON form of S-3.2.2. Throws on any deviation from the canonical form. */
export function decodeInputs(bytes, eip712) {
  const r = new R(bytes);
  const count = Number(r.uint(4));
  const out = [];
  let last = 0;
  for (let k = 0; k < count; k++) {
    const tag = Number(r.uint(1));
    if (!KIND[tag]) throw new Error(`unknown tag ${tag}`);
    if (tag < last || (tag === 1 && last === 1)) throw new Error('BLOCK_ORDER');
    last = tag;
    const p = new R(r.take(Number(r.uint(4))));
    if (tag === 4) {
      const type = TYPE_NAME[Number(p.uint(1))];
      if (!type) throw new Error('unknown typeId');
      const message = {};
      for (const f of eip712.types[type]) message[f.name] = readField(p, f.type);
      out.push({ kind: 'action', type, message, signature: hex(p.take(65)) });
    } else if (tag === 2) {
      out.push({ kind: 'deposit', payload: { depositIndex: p.uint(8).toString(), account: hex(p.take(20)), amount: p.uint(8).toString() } });
    } else if (tag === 3) {
      out.push({ kind: 'forced', payload: { forcedIndex: p.uint(8).toString(), account: hex(p.take(20)), kind: Number(p.uint(1)), deadline: p.uint(8).toString(), arg: p.uint(8).toString() } });
    } else {
      const round = p.uint(8).toString();
      const n = Number(p.uint(1));
      const observations = [];
      for (let i = 0; i < n; i++) {
        const signer = Number(p.uint(1));
        const timeMs = p.uint(8).toString();
        const pc = Number(p.uint(2));
        const prices = [];
        for (let j = 0; j < pc; j++) prices.push({ marketId: Number(p.uint(4)), price: p.uint(8).toString(), sources: Number(p.uint(1)) });
        observations.push({ signer, timeMs, prices, signature: hex(p.take(65)) });
      }
      out.push({ kind: 'oracle', payload: { round, observations } });
    }
    p.done();
  }
  r.done();
  return out;
}

// 394's signed observation (node/oracle/README.md section 3), in the engine's domain.
export const ORACLE_TYPES = {
  OracleObservation: [{ name: 'round', type: 'uint64' }, { name: 'timeMs', type: 'uint64' }, { name: 'prices', type: 'OraclePrice[]' }],
  OraclePrice: [{ name: 'marketId', type: 'uint32' }, { name: 'price', type: 'uint64' }, { name: 'sources', type: 'uint8' }],
};
