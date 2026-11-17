// Edge-to-node geo assertion: the edge module states what it observed about a request (country, subdivision,
// anonymizer signals, surface, time, request id) in one header, authenticated with HMAC-SHA256 under a secret
// shared by the edge and the node only. UNAUDITED prototype.
//
// Why (decided, technical): docs/API.md v0.1 lets the node accept a country header "from configured proxy
// addresses". A header alone is forgeable by anyone who reaches the origin directly or through a misconfigured
// proxy; an address allow-list breaks when the CDN's ranges change. The MAC makes the assertion unforgeable
// without the secret and binds it to a time window, so the node can treat it as `trusted` and still refuse
// everything else (fail closed). The node re-runs the decision itself on the asserted facts: the edge is a filter,
// the node's admission is the authority.
//
// Wire format (ASCII, fixed field order, every field present):
//   X-Postern-Geo:     v1;c=<CC|->;s=<CC-SUB|->;tor=<0|1|->;vpn=<0|1|->;host=<0|1|->;sur=<api|ui>;ts=<ms>;rid=<32 hex>
//   X-Postern-Geo-Mac: <64 hex> = HMAC-SHA256(secret, "postern-perps/geo-assertion/v1\n" + X-Postern-Geo)
// "-" means unknown. The node refuses an assertion older than MAX_AGE_MS or more than MAX_FUTURE_MS ahead of its
// clock, and a request id it has seen inside the window (replay cache, see ReplayCache).

import { hmacSha256, timingSafeEqual, hex } from './sha256.mjs';

export const HEADER = 'x-postern-geo';
export const MAC_HEADER = 'x-postern-geo-mac';
export const MAC_DOMAIN = 'postern-perps/geo-assertion/v1\n';
export const MAX_AGE_MS = 30_000;
export const MAX_FUTURE_MS = 5_000;
export const MIN_SECRET_BYTES = 32;

const enc = (s) => new TextEncoder().encode(s);
const tri = (v) => (v === true ? '1' : v === false ? '0' : '-');
const untri = (s) => (s === '1' ? true : s === '0' ? false : s === '-' ? null : undefined);

export function encodeAssertion({ country, subdivision, signals = {}, surface, tsMs, requestId }) {
  const c = country ?? '-'; const s = subdivision ?? '-';
  if (!(c === '-' || /^[A-Z][A-Z0-9]$/.test(c))) throw new TypeError('country');
  if (!(s === '-' || /^[A-Z]{2}-[A-Z0-9]{1,3}$/.test(s))) throw new TypeError('subdivision');
  if (surface !== 'api' && surface !== 'ui') throw new TypeError('surface');
  if (!Number.isSafeInteger(tsMs) || tsMs < 0) throw new TypeError('tsMs');
  if (!/^[0-9a-f]{32}$/.test(requestId)) throw new TypeError('requestId');
  return `v1;c=${c};s=${s};tor=${tri(signals.tor)};vpn=${tri(signals.vpnProxy)};host=${tri(signals.hosting)};sur=${surface};ts=${tsMs};rid=${requestId}`;
}

export function macAssertion(secret, header) {
  if (!(secret instanceof Uint8Array) || secret.length < MIN_SECRET_BYTES) throw new TypeError(`secret must be at least ${MIN_SECRET_BYTES} bytes`);
  return hex(hmacSha256(secret, enc(MAC_DOMAIN + header)));
}

const RE = /^v1;c=([A-Z][A-Z0-9]|-);s=([A-Z]{2}-[A-Z0-9]{1,3}|-);tor=([01-]);vpn=([01-]);host=([01-]);sur=(api|ui);ts=(0|[1-9][0-9]{0,15});rid=([0-9a-f]{32})$/;

/**
 * verifyAssertion(header, mac, {secret, nowMs, replay}) -> {ok: true, geo, signals, surface, tsMs, requestId}
 * or {ok: false, reason}. On ok, `geo.trusted` is true. Never throws on attacker input.
 */
export function verifyAssertion(header, mac, { secret, nowMs, replay }) {
  if (typeof header !== 'string' || typeof mac !== 'string') return { ok: false, reason: 'missing' };
  if (header.length > 200 || !/^[0-9a-f]{64}$/.test(mac)) return { ok: false, reason: 'shape' };
  const expect = macAssertion(secret, header);
  if (!timingSafeEqual(enc(expect), enc(mac))) return { ok: false, reason: 'mac' };
  const m = RE.exec(header);
  if (!m) return { ok: false, reason: 'format' };
  const tsMs = Number(m[7]);
  if (tsMs < nowMs - MAX_AGE_MS) return { ok: false, reason: 'stale' };
  if (tsMs > nowMs + MAX_FUTURE_MS) return { ok: false, reason: 'future' };
  const country = m[1] === '-' ? null : m[1];
  const subdivision = m[2] === '-' ? null : m[2];
  if (subdivision !== null && (country === null || subdivision.slice(0, 2) !== country)) return { ok: false, reason: 'inconsistent' };
  if (replay && !replay.add(m[8], tsMs, nowMs)) return { ok: false, reason: 'replay' };
  return {
    ok: true,
    geo: { country, subdivision, trusted: true },
    signals: { tor: untri(m[3]), vpnProxy: untri(m[4]), hosting: untri(m[5]) },
    surface: m[6], tsMs, requestId: m[8],
  };
}

/** Request ids seen inside the assertion window. add() returns false for a repeat. Bounded by the window. */
export class ReplayCache {
  constructor({ windowMs = MAX_AGE_MS + MAX_FUTURE_MS, maxEntries = 1_000_000 } = {}) {
    this.windowMs = windowMs; this.maxEntries = maxEntries; this.seen = new Map();
  }
  add(rid, tsMs, nowMs) {
    for (const [k, t] of this.seen) { if (t < nowMs - this.windowMs) this.seen.delete(k); else break; }
    if (this.seen.has(rid)) return false;
    if (this.seen.size >= this.maxEntries) return false; // fail closed under a flood rather than forget ids
    this.seen.set(rid, Math.max(tsMs, nowMs));
    return true;
  }
}
