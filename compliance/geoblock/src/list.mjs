// Strict loader of compliance/geoblock.json (agent 392's list). UNAUDITED prototype. Not legal advice.
//
// The loader refuses (throws ListError) every list it does not fully understand, so that a list error can only
// ever close access, never open it. It never edits the list: it returns a frozen, normalized view.
//
// Refused (each one has a test in test/list.test.mjs):
//  - wrong `schema`, or a `version` that is not MAJOR.MINOR[.PATCH], or a major version other than 0
//    (a new major is a new decision rule: this code must be updated before it is read);
//  - a top-level key that is neither a decision field nor in INFORMATIONAL_TOP_LEVEL (an unknown decision field
//    would otherwise be silently ignored);
//  - tiers other than exactly {blocked, restricted-review, allowed}; `blocked` or `restricted-review` with an
//    effect other than "deny"; `allowed` with an effect other than "allow";
//  - `default_tier` whose effect is not "deny"; `unknown_country_effect` other than "deny";
//  - a country key that is not ISO 3166-1 alpha-2 shape, a RESERVED code (XX, T1, ZZ, A1, A2, O1, EU, AP) listed
//    with an allowing tier, an entry whose tier is unknown;
//  - a subdivision key that is not ISO 3166-2 shape, or a subdivision with an allowing tier (subdivisions can only
//    restrict: the rule allows only when the COUNTRY is allowed);
//  - anonymizer values outside the known set, or a missing anonymizer;
//  - a denied policy other than "exit_only" / "actions_refused", or an exit-only action outside the closed set of
//    actions that only reduce exposure or take funds out (EXIT_ACTIONS_KNOWN).

export class ListError extends Error {
  constructor(msg) { super(`geoblock list refused: ${msg}`); this.name = 'ListError'; }
}

export const SCHEMA = 'postern-perps/geoblock';
export const SUPPORTED_MAJOR = 0;
export const TIERS = Object.freeze({ blocked: 'deny', 'restricted-review': 'deny', allowed: 'allow' });
// Codes that are not countries: missing data (XX), Tor (T1, as Cloudflare reports it), user-assigned (ZZ, used by
// the node's mock fixture), legacy GeoIP pseudo-codes (A1 anonymous proxy, A2 satellite, O1 other, EU, AP).
export const RESERVED_CODES = Object.freeze(['XX', 'T1', 'ZZ', 'A1', 'A2', 'O1', 'EU', 'AP']);
export const ANONYMIZER_VALUES = Object.freeze({
  tor: ['deny'],
  vpn_proxy: ['deny'],
  hosting: ['deny', 'deny-ui-allow-api-with-attested-account'],
});
export const EXIT_ACTIONS_KNOWN = Object.freeze([
  'CancelOrder', 'CancelAll', 'PlaceOrder(reduceOnly=true)', 'Withdraw', 'RevokeAgent',
]);
const DECISION_TOP_LEVEL = ['schema', 'version', 'rule', 'tiers', 'default_tier', 'unknown_country_effect',
  'countries', 'subdivisions', 'anonymizers', 'denied_policy'];
export const INFORMATIONAL_TOP_LEVEL = Object.freeze(['generated_utc', 'status', 'reason_categories', 'sources',
  'notes', '$comment', 'research', 'changelog']);

const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

function checkEntries(obj, keyRe, what, allowAllowing) {
  if (!isObj(obj)) throw new ListError(`${what} must be an object`);
  const out = {};
  for (const [k, e] of Object.entries(obj)) {
    if (!keyRe.test(k)) throw new ListError(`${what} key ${JSON.stringify(k)} has the wrong shape`);
    if (!isObj(e)) throw new ListError(`${what}.${k} must be an object`);
    if (!Object.hasOwn(TIERS, e.tier)) throw new ListError(`${what}.${k}.tier ${JSON.stringify(e.tier)} unknown`);
    const effect = TIERS[e.tier];
    if (effect === 'allow' && !allowAllowing(k)) throw new ListError(`${what}.${k} may not be allowing`);
    if (e.reasons !== undefined && !(Array.isArray(e.reasons) && e.reasons.every((r) => typeof r === 'string'))) {
      throw new ListError(`${what}.${k}.reasons must be an array of strings`);
    }
    out[k] = Object.freeze({ tier: e.tier, effect, reasons: Object.freeze([...(e.reasons ?? [])]), name: String(e.name ?? '') });
  }
  return Object.freeze(out);
}

/**
 * loadList(json, {sha256}) -> frozen list view. `json` is the parsed object; `sha256` (hex of the exact file
 * bytes) is recorded in every decision so that a log line names the list it was decided under.
 */
export function loadList(json, { sha256 = null } = {}) {
  if (!isObj(json)) throw new ListError('not an object');
  for (const k of Object.keys(json)) {
    if (!DECISION_TOP_LEVEL.includes(k) && !INFORMATIONAL_TOP_LEVEL.includes(k)) {
      throw new ListError(`unknown top-level key ${JSON.stringify(k)} (a decision field this code does not read)`);
    }
  }
  if (json.schema !== SCHEMA) throw new ListError(`schema must be ${SCHEMA}`);
  const m = typeof json.version === 'string' && /^(\d+)\.(\d+)(?:\.(\d+))?$/.exec(json.version);
  if (!m) throw new ListError('version must be MAJOR.MINOR[.PATCH]');
  if (Number(m[1]) !== SUPPORTED_MAJOR) throw new ListError(`major version ${m[1]} not supported (expects ${SUPPORTED_MAJOR})`);
  if (typeof json.rule !== 'string' || !/deny by default/i.test(json.rule)) throw new ListError('rule text must state "Deny by default"');
  if (!isObj(json.tiers)) throw new ListError('tiers missing');
  const tierNames = Object.keys(json.tiers).sort();
  if (JSON.stringify(tierNames) !== JSON.stringify(Object.keys(TIERS).sort())) {
    throw new ListError(`tiers must be exactly ${Object.keys(TIERS).join(', ')}`);
  }
  for (const [t, eff] of Object.entries(TIERS)) {
    if (!isObj(json.tiers[t]) || json.tiers[t].effect !== eff) throw new ListError(`tier ${t} must have effect ${eff}`);
  }
  if (!Object.hasOwn(TIERS, json.default_tier) || TIERS[json.default_tier] !== 'deny') throw new ListError('default_tier must be a denying tier');
  if (json.unknown_country_effect !== 'deny') throw new ListError('unknown_country_effect must be deny');
  const countries = checkEntries(json.countries, /^[A-Z]{2}$/, 'countries', (k) => !RESERVED_CODES.includes(k));
  const subdivisions = checkEntries(json.subdivisions ?? {}, /^[A-Z]{2}-[A-Z0-9]{1,3}$/, 'subdivisions', () => false);
  if (!isObj(json.anonymizers)) throw new ListError('anonymizers missing');
  for (const k of Object.keys(json.anonymizers)) if (!Object.hasOwn(ANONYMIZER_VALUES, k)) throw new ListError(`anonymizer ${k} unknown`);
  const anonymizers = {};
  for (const [k, vals] of Object.entries(ANONYMIZER_VALUES)) {
    if (!vals.includes(json.anonymizers[k])) throw new ListError(`anonymizers.${k} must be one of ${vals.join(' | ')}`);
    anonymizers[k] = json.anonymizers[k];
  }
  const dp = json.denied_policy;
  if (!isObj(dp) || !['exit_only', 'actions_refused'].includes(dp.policy)) throw new ListError('denied_policy.policy must be exit_only or actions_refused');
  const exitActions = dp.policy === 'exit_only' ? dp.exit_only_actions : [];
  if (!Array.isArray(exitActions)) throw new ListError('denied_policy.exit_only_actions must be an array');
  for (const a of exitActions) if (!EXIT_ACTIONS_KNOWN.includes(a)) throw new ListError(`exit-only action ${JSON.stringify(a)} is not an exit action`);
  // Subdivisions per country, for the "subdivision unknown in a country with restricted subdivisions" rule.
  const restrictedSubdivisionCountries = Object.freeze([...new Set(Object.keys(subdivisions).map((s) => s.slice(0, 2)))].sort());
  return Object.freeze({
    schema: json.schema,
    version: json.version,
    sha256,
    defaultTier: json.default_tier,
    countries,
    subdivisions,
    restrictedSubdivisionCountries,
    anonymizers: Object.freeze(anonymizers),
    deniedPolicy: dp.policy,
    exitActions: Object.freeze([...exitActions]),
  });
}
