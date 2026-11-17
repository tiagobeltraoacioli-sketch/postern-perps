// Edge module (Cloudflare-Worker-style fetch handler) for PosternDEX Perpetuals. UNAUDITED prototype.
// NOT DEPLOYED: no account, route, DNS or secret exists; this file runs only in local tests (test/edge.test.mjs).
// Not legal advice: the list (compliance/geoblock.json) carries every legal judgement.
//
// What it does, per request:
//  1. Removes every client-supplied geo header (x-postern-geo, x-postern-geo-mac, x-mock-country in live mode):
//     only the edge itself asserts geography to the origin.
//  2. Reads the edge's own observation (request.cf.country, request.cf.regionCode; CF-IPCountry "T1" = Tor,
//     "XX" = no data, per Cloudflare's HTTP headers reference) and the local VPN / datacenter / Tor signals.
//  3. GET /v1/geo: answers itself (docs/API.md section 6 shape).
//     POST /v1/actions: parses the envelope (16 KiB limit, the API's), screens the message's addresses against the
//     sanctions list, decides; a refusal is answered here with the API's 451/403/503 error body; an allowed
//     action is forwarded with the authenticated geo assertion. The node re-decides (the edge is a filter, the
//     node's admission is the authority) and also screens the recovered signer, which only it computes.
//     Other /v1/* (reads, the WebSocket upgrade): forwarded with the assertion; never geoblocked here (public reads).
//     Anything else (the static app): forwarded unchanged; the app reads /v1/geo and shows the notices.
//  4. Fail closed: no list, no sanctions list (live), or no MAC secret -> actions refused with 503, except the
//     no-value actions the decision allows; reads still pass (without an assertion, which the node needs only to
//     judge actions).

import { loadList } from '../src/list.mjs';
import { loadSanctions } from '../src/sanctions.mjs';
import { decide, geoStatus, MESSAGES } from '../src/decide.mjs';
import { encodeAssertion, macAssertion, HEADER, MAC_HEADER, MIN_SECRET_BYTES } from '../src/geo-assertion.mjs';
import { hexToBytes } from '../src/keccak.mjs';

export const MAX_BODY_BYTES = 16 * 1024;
const STRIP = [HEADER, MAC_HEADER, 'x-mock-country'];

const json = (status, body, extra = {}) => new Response(JSON.stringify(body), {
  status, headers: { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store', ...extra },
});
const apiError = (status, code, message, details = {}) => json(status, { error: { code, message, details } });

/** The edge's own observation of the client. `signalsOf(ip)` gives {tor, vpnProxy, hosting} from local lists. */
function observe(request, cf, { mode, signalsOf }) {
  let country = typeof cf?.country === 'string' ? cf.country.toUpperCase() : null;
  let subdivision = null;
  if (country && typeof cf?.regionCode === 'string' && /^[A-Z0-9]{1,3}$/.test(cf.regionCode)) subdivision = `${country}-${cf.regionCode}`;
  if (mode === 'dev') {
    const mock = request.headers.get('x-mock-country') ?? new URL(request.url).searchParams.get('country');
    if (mock && /^[A-Za-z]{2}$/.test(mock)) { country = mock.toUpperCase(); subdivision = null; }
  }
  const ip = request.headers.get('cf-connecting-ip');
  const s = signalsOf ? signalsOf(ip) : { tor: null, vpnProxy: null, hosting: null };
  const tor = country === 'T1' ? true : s.tor;
  return { geo: { country, subdivision, trusted: true }, signals: { tor, vpnProxy: s.vpnProxy, hosting: s.hosting } };
}

/** Addresses named by an action message, by role (the signer is recovered by the node, not here). */
export function messageAddresses(type, m) {
  const out = [];
  if (m && typeof m.account === 'string') out.push({ role: 'account', address: m.account });
  if (type === 'Withdraw' && m && typeof m.recipient === 'string') out.push({ role: 'recipient', address: m.recipient });
  if ((type === 'ApproveAgent' || type === 'RevokeAgent') && m && typeof m.agent === 'string') out.push({ role: 'agent', address: m.agent });
  return out;
}

async function readBody(request) {
  const len = request.headers.get('content-length');
  if (len !== null && (!/^\d+$/.test(len) || Number(len) > MAX_BODY_BYTES)) return { tooLarge: true };
  if (!request.body) return { text: '' };
  const reader = request.body.getReader();
  const chunks = []; let n = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    n += value.length;
    if (n > MAX_BODY_BYTES) { try { await reader.cancel(); } catch {} return { tooLarge: true }; }
    chunks.push(value);
  }
  const buf = new Uint8Array(n); let o = 0;
  for (const c of chunks) { buf.set(c, o); o += c.length; }
  try { return { text: new TextDecoder('utf-8', { fatal: true }).decode(buf), bytes: buf }; } catch { return { bad: true }; }
}

/**
 * createEdge(config) -> async handler(request, cf)
 * config: {mode: 'live'|'dev', list (loaded), sanctions (loaded) | null, secret: Uint8Array | null,
 *          origin: string (base URL), fetchOrigin(Request) -> Promise<Response>, now() -> ms,
 *          randomId() -> 32 hex, signalsOf(ip) -> {tor, vpnProxy, hosting} | undefined}
 */
export function createEdge(cfg) {
  const { mode, list, sanctions, secret, origin, fetchOrigin, now, randomId, signalsOf } = cfg;
  if (mode !== 'live' && mode !== 'dev') throw new TypeError('mode');
  const secretOk = secret instanceof Uint8Array && secret.length >= MIN_SECRET_BYTES;

  const forward = (request, url, obs, surface, body) => {
    const headers = new Headers(request.headers);
    for (const h of STRIP) headers.delete(h);
    if (secretOk) {
      const a = encodeAssertion({ country: obs.geo.country && /^[A-Z][A-Z0-9]$/.test(obs.geo.country) ? obs.geo.country : null, subdivision: obs.geo.subdivision, signals: obs.signals, surface, tsMs: now(), requestId: randomId() });
      headers.set(HEADER, a);
      headers.set(MAC_HEADER, macAssertion(secret, a));
    }
    const target = new URL(url.pathname + url.search, origin);
    return fetchOrigin(new Request(target, { method: request.method, headers, body: body ?? (['GET', 'HEAD'].includes(request.method) ? undefined : request.body), redirect: 'manual', duplex: 'half' }));
  };

  return async function handle(request, cf) {
    const url = new URL(request.url);
    const obs = observe(request, cf, { mode, signalsOf });
    const api = url.pathname === '/v1' || url.pathname.startsWith('/v1/');
    if (!api) return forward(request, url, obs, 'ui');
    const base = { mode, surface: 'api', geo: obs.geo, signals: obs.signals, sanctions: sanctions ?? null, nowMs: now() };
    if (url.pathname === '/v1/geo' && request.method === 'GET') {
      return json(200, geoStatus(list ?? null, base));
    }
    if (url.pathname === '/v1/actions' && request.method === 'POST') {
      const body = await readBody(request);
      if (body.tooLarge) return apiError(413, 'TOO_LARGE', 'Body over 16 KiB.');
      if (body.bad) return apiError(400, 'MALFORMED', MESSAGES.MALFORMED, { path: 'body' });
      let env;
      try { env = JSON.parse(body.text); } catch { return apiError(400, 'MALFORMED', MESSAGES.MALFORMED, { path: 'body' }); }
      if (!env || typeof env !== 'object' || typeof env.type !== 'string' || typeof env.message !== 'object' || env.message === null) {
        return apiError(400, 'MALFORMED', MESSAGES.MALFORMED, { path: 'envelope' });
      }
      const action = { type: env.type, reduceOnly: env.type === 'PlaceOrder' ? env.message.reduceOnly : undefined };
      const d = decide(list ?? null, { ...base, kind: 'action', action, addresses: messageAddresses(env.type, env.message), attestation: 'deferred' });
      if (!d.allow) {
        const message = MESSAGES[d.code] ?? MESSAGES.MALFORMED;
        return apiError(d.httpStatus, d.code, message, { country: d.country, listVersion: d.listVersion, reasonCode: d.reasons[0] ?? d.detail ?? null });
      }
      if (!secretOk && mode === 'live') return apiError(503, 'SEQUENCER_UNAVAILABLE', 'Edge not configured.', { reasonCode: 'NO_EDGE_SECRET' });
      return forward(request, url, obs, 'api', body.bytes);
    }
    return forward(request, url, obs, 'api');
  };
}

/**
 * Worker entry point (for a future deployment; the bindings are TBD-owner and are not created by this work):
 *   GEO_MODE ("live"), GEO_LIST_JSON (the exact bytes of compliance/geoblock.json), SANCTIONS_JSON
 *   (sanctions-evm.json), GEO_EDGE_SECRET (64+ hex chars, a Worker secret shared with the node), ORIGIN_URL.
 * A list or sanctions file that fails its strict loader is treated as absent (fail closed), never as empty.
 */
let cached = null;
export default {
  async fetch(request, env) {
    if (!cached) {
      let list = null; let sanctions = null; let secret = null;
      try { list = loadList(JSON.parse(env.GEO_LIST_JSON)); } catch { list = null; }
      try { sanctions = loadSanctions(JSON.parse(env.SANCTIONS_JSON)); } catch { sanctions = null; }
      try { secret = /^[0-9a-f]{64,}$/.test(env.GEO_EDGE_SECRET ?? '') ? hexToBytes('0x' + env.GEO_EDGE_SECRET) : null; } catch { secret = null; }
      cached = createEdge({
        mode: env.GEO_MODE === 'dev' ? 'dev' : 'live', list, sanctions, secret, origin: env.ORIGIN_URL,
        fetchOrigin: (r) => fetch(r), now: () => Date.now(),
        randomId: () => Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, '0')).join(''),
        signalsOf: undefined, // VPN / datacenter lists: loaded from a binding in a deployment (DESIGN.md "IP data")
      });
    }
    return cached(request, request.cf);
  },
};
