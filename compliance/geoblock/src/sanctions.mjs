// OFAC SDN digital-currency address list: extraction from the official publication, two-format cross-check,
// build, and the run-time matcher. UNAUDITED prototype. Not legal advice: what the operator must do on a hit
// (block, hold, report) is for counsel; this code only identifies addresses the publisher lists.
//
// Source (official, U.S. Treasury OFAC Sanctions List Service, read-only, anonymous):
//   SDN.XML           https://sanctionslistservice.ofac.treas.gov/api/PublicationPreview/exports/SDN.XML
//   SDN_ADVANCED.XML  https://sanctionslistservice.ofac.treas.gov/api/PublicationPreview/exports/SDN_ADVANCED.XML
// OFAC FAQ 563: an address listing "will always begin with 'Digital Currency Address', followed by a dash and the
// digital currency's symbol". FAQ 562: the listings "are not likely to be exhaustive".
//
// The builder parses BOTH formats of the same publication and refuses unless they name exactly the same
// (entry uid, currency, address) triples and the same publication date: one format misparsed, truncated or
// changed in shape cannot silently shrink the list.

import { walk } from './xml.mjs';
import { sha256, hex } from './sha256.mjs';
import { toChecksumAddress } from './keccak.mjs';

export class SanctionsError extends Error {
  constructor(msg) { super(`sanctions: ${msg}`); this.name = 'SanctionsError'; }
}

export const DCA_PREFIX = 'Digital Currency Address - ';
export const SCHEMA = 'postern-perps/sanctions-evm';
const EVM_RE = /^0x[0-9a-fA-F]{40}$/;
// Every 0x+40-hex address is screened on the Bloch L2 (chain 700770) whatever symbol it is listed under: an EVM
// key controls the same address on every EVM chain. EVM_SYMBOLS are the native symbols of EVM chains; they only
// flag anomalies (an address under them that is not EVM-shaped is reported, never dropped silently). Token
// symbols that exist on EVM and non-EVM chains (USDT, USDC: Tron, Omni, Solana...) are not anomalies either way.
export const EVM_SYMBOLS = Object.freeze(['ETH', 'ETC', 'ARB', 'BSC']);
export const MULTICHAIN_TOKEN_SYMBOLS = Object.freeze(['USDT', 'USDC']);

const local = (name) => name.slice(name.indexOf(':') + 1);

/** Parse SDN.XML text -> {publishDate 'YYYY-MM-DD', recordCount, entries, triples: [{uid, symbol, address}]} */
export function parseSdnXml(text) {
  const triples = [];
  let publishDate = null; let recordCount = null; let entries = 0;
  let cur = null; let id = null; let buf = '';
  walk(text, {
    onOpen(name, _a, path) {
      const n = local(name); buf = '';
      if (n === 'sdnEntry' && path.length === 2) { cur = { uid: null }; entries++; }
      if (n === 'id' && cur) id = { idType: null, idNumber: null };
    },
    onText(t) { buf += t; },
    onClose(name, path) {
      const n = local(name); const v = buf.trim(); buf = '';
      const parent = path.length ? local(path[path.length - 1]) : null;
      if (n === 'Publish_Date' && parent === 'publshInformation') {
        const m = /^(\d{2})\/(\d{2})\/(\d{4})$/.exec(v);
        if (!m) throw new SanctionsError(`Publish_Date ${JSON.stringify(v)} not MM/DD/YYYY`);
        publishDate = `${m[3]}-${m[1]}-${m[2]}`;
      } else if (n === 'Record_Count' && parent === 'publshInformation') {
        if (!/^\d+$/.test(v)) throw new SanctionsError('Record_Count not an integer');
        recordCount = Number(v);
      } else if (n === 'uid' && cur && parent === 'sdnEntry') {
        cur.uid = v;
      } else if (n === 'idType' && id) id.idType = v;
      else if (n === 'idNumber' && id) id.idNumber = v;
      else if (n === 'id' && id) {
        if (id.idType && id.idType.startsWith(DCA_PREFIX)) {
          if (!cur.uid) throw new SanctionsError('id before entry uid');
          if (!id.idNumber) throw new SanctionsError(`entry ${cur.uid}: digital currency id without a number`);
          triples.push({ uid: cur.uid, symbol: id.idType.slice(DCA_PREFIX.length), address: id.idNumber });
        }
        id = null;
      } else if (n === 'sdnEntry' && cur && path.length === 1) {
        if (!cur.uid) throw new SanctionsError('sdnEntry without uid');
        cur = null;
      }
    },
  });
  if (publishDate === null || recordCount === null) throw new SanctionsError('SDN.XML: publication information missing');
  if (entries !== recordCount) throw new SanctionsError(`SDN.XML: ${entries} entries but Record_Count ${recordCount} (truncated or changed shape)`);
  return { publishDate, recordCount, entries, triples };
}

/** Parse SDN_ADVANCED.XML text -> {publishDate, parties, triples} */
export function parseSdnAdvancedXml(text) {
  const featureTypes = new Map(); // id -> label
  const triples = [];
  let date = { Year: null, Month: null, Day: null };
  let parties = 0;
  let inDate = false; let ft = null; let party = null; let feature = null; let detail = false; let buf = '';
  walk(text, {
    onOpen(name, attrs, path) {
      const n = local(name); buf = '';
      if (n === 'DateOfIssue' && path.length === 2) inDate = true;
      else if (n === 'FeatureType' && local(path[path.length - 2] ?? '') === 'FeatureTypeValues') ft = attrs.ID;
      else if (n === 'DistinctParty') { party = attrs.FixedRef; parties++; if (!party) throw new SanctionsError('DistinctParty without FixedRef'); }
      else if (n === 'Feature' && party) feature = { typeId: attrs.FeatureTypeID, details: [] };
      else if (n === 'VersionDetail' && feature) detail = true;
    },
    onText(t) { buf += t; },
    onClose(name) {
      const n = local(name); const v = buf.trim(); buf = '';
      if (inDate && (n === 'Year' || n === 'Month' || n === 'Day')) date[n] = v;
      else if (n === 'DateOfIssue') inDate = false;
      else if (n === 'FeatureType' && ft !== null) {
        if (featureTypes.has(ft)) throw new SanctionsError(`duplicate FeatureType ID ${ft}`);
        featureTypes.set(ft, v); ft = null;
      } else if (n === 'VersionDetail' && detail) { feature.details.push(v); detail = false; }
      else if (n === 'Feature' && feature) {
        const label = featureTypes.get(feature.typeId);
        if (label === undefined) throw new SanctionsError(`Feature type ${feature.typeId} not declared before use`);
        if (label.startsWith(DCA_PREFIX)) {
          if (feature.details.length !== 1 || !feature.details[0]) throw new SanctionsError(`party ${party}: digital currency feature with ${feature.details.length} details`);
          triples.push({ uid: party, symbol: label.slice(DCA_PREFIX.length), address: feature.details[0] });
        }
        feature = null;
      } else if (n === 'DistinctParty') party = null;
    },
  });
  if (![date.Year, date.Month, date.Day].every((x) => /^\d+$/.test(x ?? ''))) throw new SanctionsError('SDN_ADVANCED.XML: DateOfIssue missing');
  const publishDate = `${date.Year}-${date.Month.padStart(2, '0')}-${date.Day.padStart(2, '0')}`;
  const dcaTypes = [...featureTypes.values()].filter((l) => l.startsWith(DCA_PREFIX)).length;
  if (dcaTypes === 0) throw new SanctionsError('SDN_ADVANCED.XML: no digital currency feature types declared (changed shape?)');
  return { publishDate, parties, triples };
}

const key = (t) => `${t.uid}\u0000${t.symbol}\u0000${t.address}`;

/**
 * Build the screening list from both formats. Returns the JSON object written to sanctions-evm.json.
 * `files`: [{name, url, bytes: Uint8Array}] for SDN.XML and SDN_ADVANCED.XML (hashes recorded).
 * `builtUtc`: ISO time of this verification (passed in: the builder is deterministic for given inputs).
 * `previous`: the last accepted build (or null); `acceptRemovals`: addresses the operator confirmed as delisted.
 */
export function buildSanctions({ sdn, advanced, files, builtUtc, previous = null, acceptRemovals = [] }) {
  if (sdn.publishDate !== advanced.publishDate) throw new SanctionsError(`publication dates differ: SDN.XML ${sdn.publishDate}, SDN_ADVANCED.XML ${advanced.publishDate}`);
  const a = new Set(sdn.triples.map(key)); const b = new Set(advanced.triples.map(key));
  const onlyA = [...a].filter((k) => !b.has(k)); const onlyB = [...b].filter((k) => !a.has(k));
  if (onlyA.length || onlyB.length) {
    throw new SanctionsError(`the two formats disagree: ${onlyA.length} only in SDN.XML, ${onlyB.length} only in SDN_ADVANCED.XML (first: ${JSON.stringify((onlyA[0] ?? onlyB[0]).split('\u0000'))})`);
  }
  if (a.size === 0) throw new SanctionsError('no digital currency address in the publication (refusing an empty list)');
  const evm = new Map(); const other = {}; const anomalies = []; const byType = {};
  const dupWithin = sdn.triples.length - a.size;
  if (dupWithin > 0) anomalies.push(`SDN.XML repeats ${dupWithin} identical (uid, currency, address) triple(s) (counted once)`);
  const seen = new Set();
  for (const t of sdn.triples) {
    if (seen.has(key(t))) continue;
    seen.add(key(t));
    byType[t.symbol] = (byType[t.symbol] ?? 0) + 1;
    if (EVM_RE.test(t.address)) {
      const addr = t.address.toLowerCase();
      const e = evm.get(addr) ?? { address: addr, symbols: new Set(), uids: new Set() };
      e.symbols.add(t.symbol); e.uids.add(t.uid); evm.set(addr, e);
      const body = t.address.slice(2);
      if (body !== body.toLowerCase() && body !== body.toUpperCase() && toChecksumAddress(t.address) !== t.address) {
        anomalies.push(`listed with an invalid EIP-55 checksum (screened as lowercase): ${t.address}`);
      }
      if (!EVM_SYMBOLS.includes(t.symbol) && !MULTICHAIN_TOKEN_SYMBOLS.includes(t.symbol)) anomalies.push(`EVM-shaped address listed under ${t.symbol} (screened): ${addr}`);
    } else {
      (other[t.symbol] ??= new Set()).add(t.address);
      if (EVM_SYMBOLS.includes(t.symbol)) anomalies.push(`non-EVM address under ${t.symbol} (not screenable on the L2): ${t.address}`);
    }
  }
  const evmList = [...evm.values()].sort((x, y) => (x.address < y.address ? -1 : 1))
    .map((e) => ({ address: e.address, symbols: [...e.symbols].sort(), uids: [...e.uids].sort((p, q) => Number(p) - Number(q)) }));
  const addresses = evmList.map((e) => e.address);
  const removals = previous ? previous.evm.map((e) => e.address).filter((x) => !evm.has(x)) : [];
  const unconfirmed = removals.filter((r) => !acceptRemovals.map((x) => x.toLowerCase()).includes(r));
  if (unconfirmed.length) throw new SanctionsError(`${unconfirmed.length} address(es) of the previous build are absent (delisting?): confirm each with --accept-removal after checking OFAC's recent actions: ${unconfirmed.slice(0, 5).join(', ')}`);
  if (previous && previous.source.publishDate > sdn.publishDate) throw new SanctionsError(`publication ${sdn.publishDate} is older than the previous build's ${previous.source.publishDate}`);
  return {
    schema: SCHEMA,
    version: `${sdn.publishDate}+${evmAddressesSha256(addresses).slice(0, 12)}`,
    status: 'UNAUDITED prototype output. Screening data, not legal advice; what to do on a hit is for counsel.',
    builtUtc,
    source: {
      publisher: 'U.S. Department of the Treasury, Office of Foreign Assets Control (OFAC)',
      list: 'Specially Designated Nationals and Blocked Persons List (SDN)',
      publishDate: sdn.publishDate,
      recordCount: sdn.recordCount,
      files: files.map((f) => ({ name: f.name, url: f.url, bytes: f.bytes.length, sha256: hex(sha256(f.bytes)) })),
      crossCheck: `SDN.XML and SDN_ADVANCED.XML name the same ${a.size} (uid, currency, address) triples`,
    },
    counts: { triples: a.size, duplicatesWithinSdnXml: dupWithin, byType: Object.fromEntries(Object.entries(byType).sort()), evmAddresses: addresses.length },
    evmAddressesSha256: evmAddressesSha256(addresses),
    evm: evmList,
    other: Object.fromEntries(Object.entries(other).sort().map(([k, v]) => [k, [...v].sort()])),
    removalsAccepted: removals,
    anomalies,
  };
}

export function evmAddressesSha256(addresses) {
  return hex(sha256(new TextEncoder().encode(addresses.join('\n'))));
}

/**
 * Run-time matcher. Refuses (throws) a file that is not well-formed: wrong schema, an address that is not
 * lowercase 0x+40 hex, not strictly sorted, or a content hash that does not match (a hand-edited or truncated
 * list). `maxAgeMs`: the list is "fresh" while now - builtUtc <= maxAgeMs (default 48 h: the builder runs daily,
 * one missed day is tolerated). A stale list still refuses its addresses (see decide.mjs).
 */
export function loadSanctions(json, { maxAgeMs = 48 * 3600 * 1000 } = {}) {
  if (!json || json.schema !== SCHEMA) throw new SanctionsError(`schema must be ${SCHEMA}`);
  if (!Array.isArray(json.evm)) throw new SanctionsError('evm list missing');
  const addrs = json.evm.map((e) => e.address);
  for (let i = 0; i < addrs.length; i++) {
    if (!/^0x[0-9a-f]{40}$/.test(addrs[i])) throw new SanctionsError(`address ${i} not lowercase 0x+40 hex`);
    if (i > 0 && !(addrs[i - 1] < addrs[i])) throw new SanctionsError('addresses not strictly sorted (duplicate or reordered)');
  }
  if (evmAddressesSha256(addrs) !== json.evmAddressesSha256) throw new SanctionsError('content hash mismatch');
  const builtMs = Date.parse(json.builtUtc);
  if (!Number.isFinite(builtMs)) throw new SanctionsError('builtUtc missing');
  const set = new Set(addrs);
  return Object.freeze({
    version: json.version,
    sha256: json.evmAddressesSha256,
    builtMs,
    size: set.size,
    has: (addr) => set.has(String(addr).toLowerCase()),
    fresh: (nowMs) => nowMs >= builtMs && nowMs - builtMs <= maxAgeMs,
  });
}
