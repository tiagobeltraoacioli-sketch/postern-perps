// Local IP signals for a node that is not behind the edge (or to cross-check it): country ranges (DB-IP
// "IP to Country Lite" CSV, CC BY 4.0), Tor exits (Tor Project bulk exit list), VPN and datacenter CIDRs
// (X4BNet lists_vpn, MIT, upstream IPLocate CC BY-SA 4.0). UNAUDITED prototype, no dependencies.
// Sources, licences and the update procedure: DESIGN.md "IP data". Loading is strict: an unparsable line,
// overlapping or unsorted ranges, or a range whose ends differ in family throws, so a damaged file never
// becomes a silently smaller database.

export class IpDbError extends Error { constructor(m) { super(`ipdb: ${m}`); this.name = 'IpDbError'; } }

/** Parse an IPv4 or IPv6 literal -> {v: 4|6, n: BigInt} or null. IPv4-mapped IPv6 (::ffff:a.b.c.d) -> v4. */
export function parseIp(s) {
  if (typeof s !== 'string' || s.length > 45) return null;
  const v4 = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$/.exec(s);
  if (v4) return { v: 4, n: BigInt(((+v4[1] << 24) >>> 0) + (+v4[2] << 16) + (+v4[3] << 8) + +v4[4]) };
  if (!/^[0-9a-fA-F:.]+$/.test(s) || !s.includes(':')) return null;
  const tail = [];
  let str = s;
  const lastColon = s.lastIndexOf(':');
  if (s.slice(lastColon + 1).includes('.')) {
    // embedded IPv4 in the last 32 bits: rewrite as two hex groups
    const p = parseIp(s.slice(lastColon + 1));
    if (!p || p.v !== 4) return null;
    str = s.slice(0, lastColon + 1) + (p.n >> 16n).toString(16) + ':' + (p.n & 0xffffn).toString(16);
  }
  const dbl = str.split('::');
  if (dbl.length > 2) return null;
  const grp = (x) => (x === '' ? [] : x.split(':'));
  const head = grp(dbl[0]); const rest = dbl.length === 2 ? grp(dbl[1]) : [];
  for (const g of [...head, ...rest]) if (!/^[0-9a-fA-F]{1,4}$/.test(g)) return null;
  const want = 8 - tail.length;
  let groups;
  if (dbl.length === 2) {
    const fill = want - head.length - rest.length;
    if (fill < 1) return null;
    groups = [...head, ...Array(fill).fill('0'), ...rest];
  } else {
    if (head.length !== want) return null;
    groups = head;
  }
  let n = 0n;
  for (const g of groups) n = (n << 16n) | BigInt(parseInt(g, 16));
  for (const t of tail) n = (n << 16n) | BigInt(t);
  if (n >> 32n === 0xffffn) return { v: 4, n: n & 0xffffffffn }; // ::ffff:0:0/96
  return { v: 6, n };
}

class RangeTable {
  constructor() { this.starts = []; this.ends = []; this.vals = []; }
  push(a, b, v, line) {
    if (b < a) throw new IpDbError(`line ${line}: end before start`);
    const k = this.ends.length;
    if (k && a <= this.ends[k - 1]) throw new IpDbError(`line ${line}: ranges unsorted or overlapping`);
    this.starts.push(a); this.ends.push(b); this.vals.push(v);
  }
  find(n) {
    let lo = 0; let hi = this.starts.length - 1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (n < this.starts[mid]) hi = mid - 1; else if (n > this.ends[mid]) lo = mid + 1; else return this.vals[mid];
    }
    return undefined;
  }
  get size() { return this.starts.length; }
}

/** DB-IP country CSV text ("ip_start,ip_end,CC" per line) -> lookup table. */
export function loadCountryCsv(text) {
  const t4 = new RangeTable(); const t6 = new RangeTable();
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].replace(/\r$/, '');
    if (line === '') { if (i === lines.length - 1) continue; throw new IpDbError(`line ${i + 1}: empty`); }
    const m = /^([^,]+),([^,]+),([A-Z]{2})$/.exec(line);
    if (!m) throw new IpDbError(`line ${i + 1}: not "start,end,CC"`);
    const a = parseIp(m[1]); const b = parseIp(m[2]);
    if (!a || !b || a.v !== b.v) throw new IpDbError(`line ${i + 1}: bad addresses`);
    (a.v === 4 ? t4 : t6).push(a.n, b.n, m[3], i + 1);
  }
  if (t4.size === 0) throw new IpDbError('no IPv4 range');
  return { v4: t4, v6: t6, size: t4.size + t6.size };
}

/** CIDR list text (one prefix per line; '#' comment lines allowed) -> membership set. */
export function loadCidrList(text) {
  const items = { 4: [], 6: [] };
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line === '' || line.startsWith('#')) continue;
    const m = /^([^/\s]+)(?:\/(\d{1,3}))?$/.exec(line);
    if (!m) throw new IpDbError(`cidr line ${i + 1}: shape`);
    const ip = parseIp(m[1]);
    if (!ip) throw new IpDbError(`cidr line ${i + 1}: address`);
    const bits = ip.v === 4 ? 32 : 128;
    const len = m[2] === undefined ? bits : Number(m[2]);
    if (len > bits) throw new IpDbError(`cidr line ${i + 1}: prefix length`);
    const host = (1n << BigInt(bits - len)) - 1n;
    if (ip.n & host) throw new IpDbError(`cidr line ${i + 1}: host bits set`);
    items[ip.v].push([ip.n, ip.n | host]);
  }
  const mk = (arr) => {
    arr.sort((x, y) => (x[0] < y[0] ? -1 : x[0] > y[0] ? 1 : 0));
    const t = new RangeTable();
    for (const [a, b] of arr) { // merge overlaps (lists legitimately nest prefixes)
      const k = t.ends.length;
      if (k && a <= t.ends[k - 1] + 1n) { if (b > t.ends[k - 1]) t.ends[k - 1] = b; } else t.push(a, b, true, 0);
    }
    return t;
  };
  return { v4: mk(items[4]), v6: mk(items[6]) };
}

/** Tor bulk exit list (one IP per line) -> set. */
export function loadTorList(text) {
  const set = new Set();
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line === '') continue;
    const ip = parseIp(line);
    if (!ip) throw new IpDbError(`tor line ${i + 1}: address`);
    set.add(`${ip.v}:${ip.n}`);
  }
  if (set.size === 0) throw new IpDbError('empty Tor list');
  return set;
}

/**
 * Build a lookup from the loaded sources. `countries`: one or two country tables (two independent sources:
 * a disagreement gives country null = unknown, which the decision denies). `tor`, `vpn`, `hosting`: optional;
 * a missing source gives null for that signal (unknown), which the decision denies in live mode.
 */
export function makeLookup({ countries = [], tor = null, vpn = null, hosting = null }) {
  if (countries.length < 1 || countries.length > 2) throw new IpDbError('one or two country sources');
  return function lookup(ipStr) {
    const ip = parseIp(ipStr);
    if (!ip) return { geo: { country: null, subdivision: null, trusted: true }, signals: { tor: null, vpnProxy: null, hosting: null }, note: 'unparsable ip' };
    const t = (tbl) => (ip.v === 4 ? tbl.v4 : tbl.v6);
    const cs = countries.map((c) => t(c).find(ip.n) ?? null);
    let country = cs[0];
    let note = null;
    if (cs.length === 2 && cs[0] !== cs[1]) { country = null; note = `country sources disagree: ${cs[0]} / ${cs[1]}`; }
    const inSet = (s) => (s === null ? null : t(s).find(ip.n) === true);
    return {
      geo: { country, subdivision: null, trusted: true },
      signals: { tor: tor === null ? null : tor.has(`${ip.v}:${ip.n}`), vpnProxy: inSet(vpn), hosting: inSet(hosting) },
      note,
    };
  };
}
