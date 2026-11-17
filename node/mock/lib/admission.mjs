// Admission of POST /v1/actions (API.md 5.1, 6, 7): parse, convert to the typed message, recover the signer,
// check agent rights, sanctions, geoblocking, sequencer and market state, nonce. UNAUDITED prototype.
//
// The rule that matters most: the signature is checked over the TYPED MESSAGE the engine applies. A request in
// human units is converted to scaled integers first and the signature must verify over the converted message, so
// a server can never apply a different number from the one the user signed (a mis-conversion only makes the
// signature fail).
import { keccak256, hex, unhex, concat } from './keccak.mjs';
import { recover } from './secp256k1.mjs';
import { typedDataHash } from './eip712.mjs';
import { parseFixed } from './fixed.mjs';

export class ApiError extends Error {
  constructor(http, code, message, details = {}) {
    super(message);
    this.http = http;
    this.code = code;
    this.details = details;
  }
}
const malformed = (path, message) => new ApiError(400, 'MALFORMED', message, { path });

const ADDRESS = /^0x[0-9a-fA-F]{40}$/;
const HUMAN = {
  PlaceOrder: { side: 'isBuy', orderType: 'orderType', timeInForce: 'timeInForce', price: 'price', size: 'size', triggerPrice: 'triggerPrice' },
};

/** Typed message from a request message (wire integers, or human units for the fields API.md 5.1 names). */
export function toTyped(x, type, message, wire) {
  const fields = x.eip712.types[type];
  if (!fields) throw malformed('type', `unknown action type ${type}`);
  if (typeof message !== 'object' || message === null || Array.isArray(message)) throw malformed('message', 'message must be an object');
  const out = {};
  const m = x.byId.get(Number(message.marketId));
  const scaleOf = (field) => {
    const s = x.eip712.scales?.[field];
    if (typeof s === 'number') return s;
    if (!m) throw malformed('message.marketId', 'unknown marketId');
    return field === 'size' ? m.sd : m.pd;
  };
  const enums = x.eip712.enums ?? {};
  const allowed = new Set(fields.map((f) => f.name));
  if (!wire && type in HUMAN) for (const k of Object.keys(HUMAN[type])) allowed.add(k);
  for (const k of Object.keys(message)) if (!allowed.has(k)) throw malformed(`message.${k}`, `unknown field ${k}`);
  for (const f of fields) {
    let v = message[f.name];
    const path = `message.${f.name}`;
    if (!wire) {
      if (type === 'PlaceOrder' && f.name === 'isBuy' && 'side' in message) {
        if (message.side !== 'buy' && message.side !== 'sell') throw malformed('message.side', 'side is buy or sell');
        if ('isBuy' in message) throw malformed('message.isBuy', 'give side or isBuy, not both');
        v = message.side === 'buy';
      } else if (type === 'PlaceOrder' && (f.name === 'orderType' || f.name === 'timeInForce') && typeof v === 'string' && !/^\d+$/.test(v)) {
        const e = enums[f.name] ?? {};
        if (!(v in e)) throw malformed(path, `unknown ${f.name} ${v}`);
        v = e[v];
      } else if (type === 'PlaceOrder' && ['price', 'size', 'triggerPrice'].includes(f.name)) {
        try {
          v = String(parseFixed(v, scaleOf(f.name === 'size' ? 'size' : 'price')));
        } catch (e) {
          throw malformed(path, e.message);
        }
      } else if ((type === 'Withdraw' || type === 'UpdateMargin') && f.name === 'amount') {
        try {
          v = String(parseFixed(v, 6));
        } catch (e) {
          throw malformed(path, e.message);
        }
      }
    }
    if (v === undefined) throw malformed(path, `missing field ${f.name}`);
    if (f.type === 'address') {
      if (typeof v !== 'string' || !ADDRESS.test(v)) throw malformed(path, 'address expected');
      v = v.toLowerCase();
    } else if (f.type === 'bytes32') {
      if (typeof v !== 'string' || !/^0x[0-9a-fA-F]{64}$/.test(v)) throw malformed(path, 'bytes32 expected');
      v = v.toLowerCase();
    } else if (f.type === 'bool') {
      if (typeof v !== 'boolean') throw malformed(path, 'boolean expected');
    } else if (f.type === 'string') {
      if (typeof v !== 'string') throw malformed(path, 'string expected');
    } else if (/^u?int\d+$/.test(f.type)) {
      if (typeof v === 'number' && Number.isSafeInteger(v)) v = String(v);
      if (typeof v !== 'string' || !/^(0|-?[1-9]\d*)$/.test(v)) throw malformed(path, `${f.type} as a canonical decimal string expected`);
    }
    out[f.name] = v;
  }
  return out;
}

/** Geoblocking decision (compliance/geoblock.json, deny by default) plus the mock's development allow-list. */
export function geoDecision(geo, mockGeo, country) {
  const c = country && /^[A-Z]{2}$/.test(country) ? country : null;
  const entry = c ? geo.countries[c] : null;
  let allowed = false;
  let reason = 'JURISDICTION';
  if (!c) reason = 'COUNTRY_UNKNOWN';
  else if (entry) allowed = geo.tiers?.[entry.tier] === 'allow' || entry.tier === 'allowed';
  else allowed = mockGeo.mock_allowed_countries.includes(c); // never overrides a listed entry (deny wins)
  return { country: c, allowed, reason: allowed ? null : reason, policy: allowed ? 'open' : geo.denied_policy?.policy ?? 'actions_refused', listVersion: geo.version };
}
export function isExitOnly(type, msg) {
  if (type === 'CancelOrder' || type === 'CancelAll' || type === 'Withdraw') return true;
  return type === 'PlaceOrder' && msg.reduceOnly === true;
}

/**
 * Admit one action. ctx: { country, sanctioned:Set, nonceWindowMs|null, now }. Returns the queued action.
 * Throws ApiError with the HTTP status and code of API.md section 7.
 */
export function admit(x, req, ctx) {
  if (typeof req !== 'object' || req === null || Array.isArray(req)) throw malformed('', 'body must be an object');
  for (const k of Object.keys(req)) if (!['type', 'message', 'signature', 'wire'].includes(k)) throw malformed(k, `unknown field ${k}`);
  const { type, message, signature } = req;
  if (typeof type !== 'string' || !x.eip712.actions.includes(type)) throw malformed('type', 'unknown action type');
  if (req.wire !== undefined && typeof req.wire !== 'boolean') throw malformed('wire', 'boolean expected');
  if (typeof signature !== 'string' || !/^0x[0-9a-fA-F]{130}$/.test(signature)) throw new ApiError(400, 'BAD_SIGNATURE', 'signature must be 65 bytes of hex');
  if (x.sequencerState !== 'running') throw new ApiError(503, 'SEQUENCER_UNAVAILABLE', `sequencer is ${x.sequencerState}`);
  const msg = toTyped(x, type, message, req.wire === true);
  const digest = typedDataHash(x.eip712.domain, type, x.eip712.types[type], msg);
  let signer;
  try {
    signer = recover(digest, unhex(signature));
  } catch (e) {
    throw new ApiError(400, 'BAD_SIGNATURE', e.message);
  }
  const account = msg.account;
  const acc = x.account(account);
  if (signer !== account) {
    const ag = acc.agents.get(signer);
    if (!ag || ag.expiresAt <= x.time) throw new ApiError(403, 'UNKNOWN_SIGNER', 'signer is neither the account nor an approved, unexpired agent', { signer });
    if (!x.eip712.agentAllowed.includes(type)) throw new ApiError(403, 'AGENT_NOT_ALLOWED', `${type} needs the main key`);
  }
  const sanctioned = [account, signer, type === 'Withdraw' ? msg.recipient : null, type === 'ApproveAgent' ? msg.agent : null].filter(Boolean).find((a) => ctx.sanctioned.has(a));
  if (sanctioned) throw new ApiError(451, 'SANCTIONED', 'address on the operator sanctions list', { address: sanctioned });
  const g = ctx.geo;
  if (!g.allowed && !(g.policy === 'exit_only' && isExitOnly(type, msg))) {
    throw new ApiError(451, 'GEO_RESTRICTED', 'PosternDEX Perpetuals is not available in your jurisdiction.', { country: g.country, listVersion: g.listVersion, policy: g.policy });
  }
  if (type === 'PlaceOrder' || type === 'UpdateLeverage' || type === 'UpdateMargin') {
    const m = x.byId.get(Number(msg.marketId));
    if (!m) throw new ApiError(404, 'NOT_FOUND', 'unknown marketId');
    if (type === 'PlaceOrder' && (m.status === 'halted' || m.status === 'settled')) throw new ApiError(409, 'MARKET_HALTED', `${m.symbol} is ${m.status}`, { reason: m.haltReason });
    if (type === 'PlaceOrder' && m.status === 'closeOnly' && msg.reduceOnly !== true) throw new ApiError(409, 'MARKET_HALTED', `${m.symbol} accepts reduce-only orders only`);
  }
  const nonce = BigInt(msg.nonce);
  const prev = x.nonceBySigner.get(signer) ?? -1n;
  if (nonce <= prev) throw new ApiError(400, 'BAD_NONCE', 'nonce not above the signer\'s highest used', { highest: String(prev) });
  if (ctx.nonceWindowMs) {
    const [back, ahead] = ctx.nonceWindowMs;
    if (nonce < BigInt(x.time - back) || nonce > BigInt(x.time + ahead)) throw new ApiError(400, 'BAD_NONCE', 'nonce outside the time window', { time: x.time });
  }
  x.nonceBySigner.set(signer, nonce);
  const actionHash = hex(keccak256(concat(digest, unhex(signer))));
  return { type, msg, wireMessage: msg, account, signer, signature: signature.toLowerCase(), actionHash, receivedAt: ctx.now, digest: hex(digest) };
}
