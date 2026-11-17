import test from 'node:test';
import assert from 'node:assert/strict';
import { parseIp, loadCountryCsv, loadCidrList, loadTorList, makeLookup, IpDbError } from '../src/ipdb.mjs';
import { decide } from '../src/decide.mjs';
import { readText, fixtureList, fixtureSanctions } from './helpers.mjs';

test('parseIp: IPv4, IPv6, mapped, refusals', () => {
  assert.deepEqual(parseIp('8.8.8.8'), { v: 4, n: 0x08080808n });
  assert.deepEqual(parseIp('255.255.255.255'), { v: 4, n: 0xffffffffn });
  assert.deepEqual(parseIp('::1'), { v: 6, n: 1n });
  assert.deepEqual(parseIp('::'), { v: 6, n: 0n });
  assert.deepEqual(parseIp('2001:db8::1'), { v: 6, n: 0x20010db8000000000000000000000001n });
  assert.deepEqual(parseIp('2001:470:37d:ffff:ffff:ffff:ffff:ffff'), { v: 6, n: 0x20010470037dffffffffffffffffffffn });
  assert.deepEqual(parseIp('::ffff:1.178.32.5'), { v: 4, n: 0x01b22005n }, 'IPv4-mapped is treated as IPv4');
  assert.deepEqual(parseIp('::FFFF:0102:0304'), { v: 4, n: 0x01020304n });
  assert.deepEqual(parseIp('64:ff9b::1.2.3.4'), { v: 6, n: 0x0064ff9b000000000000000001020304n });
  for (const bad of ['', '1.2.3', '1.2.3.4.5', '256.1.1.1', '01.2.3.4', '1.2.3.4 ', ':::', '1::2::3', '12345::', 'g::1', '1:2:3:4:5:6:7:8:9', '1:2:3:4:5:6:7::8:9', null, '::ffff:1.2.3']) {
    assert.equal(parseIp(bad), null, String(bad));
  }
});

test('country CSV excerpt (DB-IP Lite 2026-11) and two-source disagreement', () => {
  const t = loadCountryCsv(readText('fixtures/dbip-country-excerpt.csv'));
  assert.equal(t.size, 9);
  const look = makeLookup({ countries: [t] });
  assert.equal(look('1.178.32.10').geo.country, 'BR');
  assert.equal(look('1.178.47.255').geo.country, 'BR');
  assert.equal(look('1.178.48.0').geo.country, null, 'gap = unknown');
  assert.equal(look('2.152.252.1').geo.country, 'PA');
  assert.equal(look('2001:470:37d::5').geo.country, 'BR');
  assert.equal(look('::ffff:2.19.188.7').geo.country, 'UA');
  assert.equal(look('2.19.188.7').geo.subdivision, null, 'the lite database has no region');
  const other = loadCountryCsv('1.178.32.0,1.178.47.255,AR\n');
  const two = makeLookup({ countries: [t, other] });
  const r = two('1.178.32.10');
  assert.equal(r.geo.country, null);
  assert.match(r.note, /disagree: BR \/ AR/);
  const p = two('2.152.252.1'); assert.equal(p.geo.country, null, 'absent in the second source = disagreement too');
});

test('country CSV refusals', () => {
  assert.throws(() => loadCountryCsv('1.0.0.0,1.0.0.255,AU\n1.0.0.128,1.0.1.0,AU\n'), /overlapping/);
  assert.throws(() => loadCountryCsv('1.0.1.0,1.0.1.255,AU\n1.0.0.0,1.0.0.255,AU\n'), /unsorted/);
  assert.throws(() => loadCountryCsv('1.0.0.9,1.0.0.1,AU\n'), /end before start/);
  assert.throws(() => loadCountryCsv('1.0.0.0,::1,AU\n'), /bad addresses/);
  assert.throws(() => loadCountryCsv('1.0.0.0,1.0.0.255,aus\n'), /not "start,end,CC"/);
  assert.throws(() => loadCountryCsv('1.0.0.0,1.0.0.255,AU\n\n1.0.1.0,1.0.1.255,AU\n'), /empty/);
  assert.throws(() => loadCountryCsv(''), IpDbError);
});

test('Tor, VPN and datacenter signals', () => {
  const look = makeLookup({
    countries: [loadCountryCsv(readText('fixtures/dbip-country-excerpt.csv'))],
    tor: loadTorList(readText('fixtures/tor-excerpt.txt')),
    vpn: loadCidrList(readText('fixtures/vpn-excerpt.txt')),
    hosting: loadCidrList(readText('fixtures/datacenter-excerpt.txt')),
  });
  assert.equal(look('171.25.193.25').signals.tor, true);
  assert.equal(look('171.25.193.26').signals.tor, false);
  assert.equal(look('2.26.11.255').signals.vpnProxy, true);
  assert.equal(look('2.26.12.0').signals.vpnProxy, false);
  assert.equal(look('2001:550:1d05:1::9').signals.vpnProxy, true);
  assert.equal(look('1.15.255.255').signals.hosting, true);
  assert.equal(look('8.242.240.1').signals.hosting, false);
  const partial = makeLookup({ countries: [loadCountryCsv(readText('fixtures/dbip-country-excerpt.csv'))] });
  assert.deepEqual(partial('8.242.240.1').signals, { tor: null, vpnProxy: null, hosting: null }, 'missing source = unknown');
});

test('CIDR refusals and merging', () => {
  assert.throws(() => loadCidrList('1.2.3.4/24\n'), /host bits/);
  assert.throws(() => loadCidrList('1.2.3.0/33\n'), /prefix length/);
  assert.throws(() => loadCidrList('1.2.3.0/24 extra\n'), /shape/);
  const s = loadCidrList('# c\n10.0.0.0/8\n10.1.0.0/16\n11.0.0.0/8\n');
  assert.equal(s.v4.size, 1, 'nested and adjacent prefixes merge');
});

test('local lookup feeds the decision; missing signal sources deny in live mode', () => {
  const list = fixtureList(); const sanctions = fixtureSanctions();
  const csv = loadCountryCsv(readText('fixtures/dbip-country-excerpt.csv'));
  const full = makeLookup({ countries: [csv], tor: loadTorList(readText('fixtures/tor-excerpt.txt')), vpn: loadCidrList(readText('fixtures/vpn-excerpt.txt')), hosting: loadCidrList(readText('fixtures/datacenter-excerpt.txt')) });
  const req = (r) => ({ mode: 'live', surface: 'api', kind: 'action', action: { type: 'PlaceOrder', reduceOnly: false }, ...r,
    addresses: [{ role: 'account', address: '0x00000000000000000000000000000000000000a1' }], sanctions, nowMs: Date.parse('2026-11-17T01:00:00Z'), attestation: { status: 'valid', residenceCountry: 'PA' } });
  assert.equal(decide(list, req(full('2.152.252.1'))).code, 'OK', 'PA residential');
  assert.equal(decide(list, req(full('1.178.32.10'))).code, 'GEO_RESTRICTED', 'BR');
  assert.deepEqual(decide(list, req(full('2.19.188.7'))).reasons, ['SUBDIVISION_UNKNOWN'], 'UA without region from the lite database');
  const partial = makeLookup({ countries: [csv] });
  assert.deepEqual(decide(list, req(partial('2.152.252.1'))).reasons, ['SIGNALS_UNAVAILABLE']);
});
