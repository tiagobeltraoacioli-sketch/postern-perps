// The single access decision of PosternDEX Perpetuals, shared by the edge module, the node's admission (through
// the Rust port in ../rust, held to the same vectors) and the frontend's display. UNAUDITED prototype. Not legal
// advice: the list (agent 392's compliance/geoblock.json) carries every legal judgement; this code only applies it.
//
// Pure: no clock, no I/O, no randomness. Same inputs -> same decision on every implementation
// (vectors/decision-vectors.json is the conformance suite).
//
// Order of evaluation (first matching refusal wins; every applicable reason is still listed in `reasons`):
//   1. request shape (mode, kind, action type, addresses)            -> 400 MALFORMED
//   2. sanctions: any screened address on the list                    -> 451 SANCTIONED (everything, exits too)
//      sanctions list missing or stale in live mode                   -> 503 SANCTIONS_UNAVAILABLE, except actions
//                                                                        that move no value (cancel, revoke)
//   3. kind "read"                                                    -> allowed (public reads are not geoblocked)
//      kind "deposit" (an L2 vault event: no IP exists)               -> allowed when not sanctioned
//   4. geography + anonymizers -> geo-denied or geo-allowed
//   5. geo-denied: exit action under policy exit_only                 -> allowed, exitOnly = true
//                  anything else                                       -> 451 GEO_RESTRICTED
//   6. geo-allowed: attestation required (live default) for actions that open or change exposure
//                                                                      -> 403 ATTESTATION_REQUIRED when missing

import { RESERVED_CODES, TIERS } from './list.mjs';
import { normalizeEvmAddress } from './keccak.mjs';

export const ACTION_TYPES = Object.freeze(['PlaceOrder', 'CancelOrder', 'CancelAll', 'UpdateLeverage',
  'UpdateMargin', 'Withdraw', 'ApproveAgent', 'RevokeAgent']);
// Actions that move no value and add no exposure: still allowed while the sanctions list is unavailable.
export const NO_VALUE_ACTIONS = Object.freeze(['CancelOrder', 'CancelAll', 'RevokeAgent']);
export const ADDRESS_ROLES = Object.freeze(['signer', 'account', 'recipient', 'agent', 'depositor']);

export const MESSAGES = Object.freeze({
  GEO_RESTRICTED: 'PosternDEX Perpetuals is not available in your jurisdiction.',
  EXIT_ONLY: 'PosternDEX Perpetuals is not available in your jurisdiction. You can still cancel orders, reduce positions and withdraw.',
  SANCTIONED: 'This address cannot use PosternDEX Perpetuals.',
  SANCTIONS_UNAVAILABLE: 'Screening is temporarily unavailable; only cancellations are accepted.',
  ATTESTATION_REQUIRED: 'Confirm the eligibility statement before trading.',
  MALFORMED: 'The request is malformed.',
  LIST_UNAVAILABLE: 'Access rules are unavailable; only cancellations are accepted.',
});

const refuse = (base, code, httpStatus, detail) => ({ ...base, allow: false, code, httpStatus, detail });

/**
 * Evaluate geography and anonymizer signals. Returns {denied, reasons[], country, subdivision, tier}.
 * `geo.trusted` is true only for a country from the authenticated edge assertion or the local IP database;
 * in live mode an untrusted country (a client header, a mock query) is treated as unknown.
 */
export function evaluateGeo(list, req) {
  const live = req.mode === 'live';
  const geo = req.geo ?? {};
  const reasons = [];
  // ASCII only before case folding (a non-ASCII letter can upper-case into ASCII, e.g. U+00DF -> "SS").
  const ascii = (v) => typeof v === 'string' && /^[\x21-\x7e]+$/.test(v);
  let country = ascii(geo.country) ? geo.country.toUpperCase() : null;
  let subdivision = ascii(geo.subdivision) ? geo.subdivision.toUpperCase() : null;
  if (live && geo.trusted !== true) { reasons.push('UNTRUSTED_GEO_SOURCE'); country = null; subdivision = null; }
  let tier = null;
  if (country === 'T1') reasons.push('ANONYMIZER_TOR');
  if (country === null || !/^[A-Z]{2}$/.test(country) || RESERVED_CODES.includes(country)) {
    reasons.push('UNKNOWN_COUNTRY');
  } else {
    const entry = Object.hasOwn(list.countries, country) ? list.countries[country] : null;
    tier = entry ? entry.tier : 'unlisted';
    const effect = entry ? entry.effect : TIERS[list.defaultTier];
    if (effect !== 'allow') reasons.push('JURISDICTION');
    if (subdivision !== null) {
      if (!/^[A-Z]{2}-[A-Z0-9]{1,3}$/.test(subdivision) || subdivision.slice(0, 2) !== country) {
        reasons.push('GEO_INCONSISTENT');
      } else if (Object.hasOwn(list.subdivisions, subdivision)) {
        reasons.push('JURISDICTION_SUBDIVISION');
      }
    } else if (list.restrictedSubdivisionCountries.includes(country)) {
      // The country has restricted regions and the region is unknown: deny (decided, technical; see DESIGN.md).
      reasons.push('SUBDIVISION_UNKNOWN');
    }
  }
  // A valid attestation declaring a residence the list denies closes access whatever the IP says (a resident
  // travelling to an allowed country, or using one): the declaration can only add a refusal, never remove one.
  const att = req.attestation;
  if (att && att.status === 'valid' && typeof att.residenceCountry === 'string') {
    const e = Object.hasOwn(list.countries, att.residenceCountry) ? list.countries[att.residenceCountry] : null;
    const eff = e ? e.effect : TIERS[list.defaultTier];
    if (eff !== 'allow') reasons.push('RESIDENCE_DECLARED_RESTRICTED');
  }
  const sig = req.signals ?? {};
  for (const [k, code] of [['tor', 'ANONYMIZER_TOR'], ['vpnProxy', 'ANONYMIZER_VPN'], ['hosting', 'ANONYMIZER_HOSTING']]) {
    const v = sig[k];
    if (v === true) {
      if (k === 'hosting' && list.anonymizers.hosting === 'deny-ui-allow-api-with-attested-account'
          && req.surface === 'api' && (req.attestation === 'deferred' || attestedResidenceAllowed(list, req.attestation))) {
        // hosting IP on the API with a valid attestation of an allowed residence; 'deferred' = the edge, which
        // holds no attestation records, passes the request on with host=1 and the node judges the attestation
        continue;
      }
      if (!reasons.includes(code)) reasons.push(code);
    } else if (v !== false && live) {
      reasons.push('SIGNALS_UNAVAILABLE');
    }
  }
  return { denied: reasons.length > 0, reasons: [...new Set(reasons)], country, subdivision, tier };
}

function attestedResidenceAllowed(list, att) {
  if (!att || att.status !== 'valid' || typeof att.residenceCountry !== 'string') return false;
  const c = att.residenceCountry;
  const e = Object.hasOwn(list.countries, c) ? list.countries[c] : null;
  return !!e && e.effect === 'allow' && !list.restrictedSubdivisionCountries.includes(c);
}

function isExitAction(list, action) {
  if (list.deniedPolicy !== 'exit_only') return false;
  if (action.type === 'PlaceOrder') return action.reduceOnly === true && list.exitActions.includes('PlaceOrder(reduceOnly=true)');
  return list.exitActions.includes(action.type);
}

/**
 * decide(list, req) -> Decision
 * req: { mode: 'live'|'dev', surface: 'api'|'ui', kind: 'action'|'read'|'deposit',
 *        action?: {type, reduceOnly?}, geo?: {country, subdivision, trusted}, signals?: {tor, vpnProxy, hosting},
 *        addresses?: [{role, address}], sanctions?: SanctionsSet|null, nowMs?: number,
 *        attestation?: {status: 'valid'|'missing'|'invalid'|'expired', residenceCountry?}|'deferred'|null,
 *          ('deferred' only at the edge: the node, which holds the records, re-decides with the real value),
 *        requireAttestation?: boolean (default: true in live, false in dev) }
 * `list` may be null (missing or refused list): then only reads and no-value actions pass.
 */
export function decide(list, req) {
  const mode = req?.mode;
  const base = {
    allow: false, code: 'MALFORMED', httpStatus: 400, exitOnly: false, restricted: null, sanctioned: false,
    policy: null, country: null, subdivision: null, tier: null, reasons: [],
    listVersion: list?.version ?? null, listSha256: list?.sha256 ?? null,
    sanctionsVersion: req?.sanctions?.version ?? null, sanctionsSha256: req?.sanctions?.sha256 ?? null, detail: null,
  };
  if (mode !== 'live' && mode !== 'dev') return refuse(base, 'MALFORMED', 400, 'mode');
  if (!['action', 'read', 'deposit'].includes(req.kind)) return refuse(base, 'MALFORMED', 400, 'kind');
  if (req.surface !== 'api' && req.surface !== 'ui') return refuse(base, 'MALFORMED', 400, 'surface');
  let action = null;
  if (req.kind === 'action') {
    action = req.action;
    if (!action || !ACTION_TYPES.includes(action.type)) return refuse(base, 'MALFORMED', 400, 'action.type');
    if (action.type === 'PlaceOrder' && typeof action.reduceOnly !== 'boolean') return refuse(base, 'MALFORMED', 400, 'action.reduceOnly');
  }
  // 1-2. addresses and sanctions
  const addrs = req.addresses ?? [];
  if (!Array.isArray(addrs)) return refuse(base, 'MALFORMED', 400, 'addresses');
  const normalized = [];
  for (const a of addrs) {
    if (!a || !ADDRESS_ROLES.includes(a.role)) return refuse(base, 'MALFORMED', 400, 'address.role');
    const n = normalizeEvmAddress(a.address);
    if (n === null) return refuse(base, 'MALFORMED', 400, `address.${a.role}`);
    normalized.push({ role: a.role, address: n });
  }
  if ((req.kind === 'action' || req.kind === 'deposit') && normalized.length === 0) return refuse(base, 'MALFORMED', 400, 'no address to screen');
  const s = req.sanctions ?? null;
  const sanctionsUsable = s !== null && (mode === 'dev' || (typeof req.nowMs === 'number' && s.fresh(req.nowMs)));
  if (s !== null) {
    const hits = normalized.filter((a) => s.has(a.address));
    if (hits.length > 0) {
      // A hit is honoured even from a stale list: staleness can only add refusals, never remove one.
      return { ...refuse(base, 'SANCTIONED', 451, hits.map((h) => h.role).join(',')), sanctioned: true, reasons: ['SANCTIONED'] };
    }
  }
  if (req.kind === 'read') return { ...base, allow: true, code: 'OK', httpStatus: 200 };
  if (!sanctionsUsable && mode === 'live') {
    if (req.kind === 'deposit' || !NO_VALUE_ACTIONS.includes(action.type)) {
      return { ...refuse(base, 'SANCTIONS_UNAVAILABLE', 503, s === null ? 'no list' : 'stale'), reasons: ['SANCTIONS_UNAVAILABLE'] };
    }
  }
  if (req.kind === 'deposit') return { ...base, allow: true, code: 'OK', httpStatus: 200 };
  if (list === null || list === undefined) {
    if (NO_VALUE_ACTIONS.includes(action.type)) return { ...base, allow: true, code: 'OK_LIST_UNAVAILABLE', httpStatus: 200, reasons: ['LIST_UNAVAILABLE'] };
    return { ...refuse(base, 'LIST_UNAVAILABLE', 503, null), reasons: ['LIST_UNAVAILABLE'] };
  }
  // 4. geography
  const g = evaluateGeo(list, req);
  const out = { ...base, country: g.country, subdivision: g.subdivision, tier: g.tier, reasons: g.reasons, restricted: g.denied };
  if (g.denied) {
    out.policy = list.deniedPolicy;
    if (isExitAction(list, action)) return { ...out, allow: true, code: 'OK_EXIT_ONLY', httpStatus: 200, exitOnly: true };
    return refuse(out, 'GEO_RESTRICTED', 451, g.reasons[0]);
  }
  out.policy = 'open';
  // 6. attestation for actions that open or change exposure (exit actions never need it)
  const requireAtt = typeof req.requireAttestation === 'boolean' ? req.requireAttestation : mode === 'live';
  const exitLike = action.type === 'PlaceOrder' ? action.reduceOnly === true : ['CancelOrder', 'CancelAll', 'Withdraw', 'RevokeAgent'].includes(action.type);
  if (requireAtt && !exitLike && req.attestation !== 'deferred' && req.attestation?.status !== 'valid') {
    return { ...refuse(out, 'ATTESTATION_REQUIRED', 403, req.attestation?.status ?? 'missing'), reasons: ['ATTESTATION_REQUIRED'] };
  }
  return { ...out, allow: true, code: 'OK', httpStatus: 200 };
}

/** GET /v1/geo body (docs/API.md section 6 shape), from the same evaluation. */
export function geoStatus(list, req) {
  if (!list) {
    return { country: null, restricted: true, policy: 'actions_refused', listVersion: null, reasonCode: 'LIST_UNAVAILABLE', message: MESSAGES.LIST_UNAVAILABLE, sanctioned: false };
  }
  const g = evaluateGeo(list, req);
  const policy = g.denied ? list.deniedPolicy : 'open';
  return {
    country: g.country,
    subdivision: g.subdivision,
    restricted: g.denied,
    policy,
    listVersion: list.version,
    reasonCode: g.denied ? g.reasons[0] : null,
    reasons: g.reasons,
    message: g.denied ? (policy === 'exit_only' ? MESSAGES.EXIT_ONLY : MESSAGES.GEO_RESTRICTED) : null,
    sanctioned: false,
  };
}
