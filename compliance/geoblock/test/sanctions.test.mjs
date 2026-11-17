import test from 'node:test';
import assert from 'node:assert/strict';
import { parseSdnXml, parseSdnAdvancedXml, buildSanctions, loadSanctions, evmAddressesSha256, SanctionsError } from '../src/sanctions.mjs';
import { XmlError, walk } from '../src/xml.mjs';
import { readText, readJson } from './helpers.mjs';

const sdnText = readText('fixtures/sdn-excerpt.xml');
const advText = readText('fixtures/sdn-advanced-excerpt.xml');
const files = [{ name: 'SDN.XML', url: 'u1', bytes: new TextEncoder().encode(sdnText) }, { name: 'SDN_ADVANCED.XML', url: 'u2', bytes: new TextEncoder().encode(advText) }];
const build = (o = {}) => buildSanctions({ sdn: parseSdnXml(o.sdn ?? sdnText), advanced: parseSdnAdvancedXml(o.adv ?? advText), files, builtUtc: '2026-11-17T00:00:00Z', ...o.extra });

test('both formats of the excerpt agree and give the committed fixture', () => {
  const s = parseSdnXml(sdnText); const a = parseSdnAdvancedXml(advText);
  assert.equal(s.publishDate, '2026-11-13'); assert.equal(a.publishDate, '2026-11-13');
  assert.equal(s.entries, 4); assert.equal(s.triples.length, 11); assert.equal(a.triples.length, 11);
  const out = build();
  const committed = readJson('fixtures/sanctions-fixture.json');
  out.source.files = committed.source.files; // the fixture was built with the real URLs
  assert.deepEqual(out, committed);
  assert.deepEqual(out.evm.map((e) => e.address), ['0x0931ca4d13bb4ba75d9b7132ab690265d749a5e7', '0x19f8f2b0915daa12a3f5c9cf01df9e24d53794f7', '0x983a81ca6fb1e441266d2fbcb7d8e530ac2e05a2']);
  assert.deepEqual(out.evm[2].symbols, ['ETH', 'USDC', 'USDT']);
});

test('an address present in only one format refuses the build', () => {
  const adv = advText.replace('0x19F8f2B0915Daa12a3f5C9CF01dF9E24D53794F7', '0x19F8f2B0915Daa12a3f5C9CF01dF9E24D53794F8');
  assert.notEqual(adv, advText);
  assert.throws(() => build({ adv }), /formats disagree: 1 only in SDN.XML, 1 only in SDN_ADVANCED.XML/);
  const sdn = sdnText.replace(/<id>\s*<uid>\d+<\/uid>\s*<idType>Digital Currency Address - ETH<\/idType>\s*<idNumber>0x0931ca4d13bb4ba75d9b7132ab690265d749a5e7<\/idNumber>\s*<\/id>/i, '');
  assert.notEqual(sdn, sdnText);
  assert.throws(() => build({ sdn }), /formats disagree: 0 only in SDN.XML, 1 only/);
});

test('publication dates that differ refuse the build', () => {
  assert.throws(() => build({ sdn: sdnText.replace('<Publish_Date>11/13/2026', '<Publish_Date>11/12/2026') }), /publication dates differ/);
});

test('a truncated SDN.XML (entry count below Record_Count) is refused', () => {
  assert.throws(() => parseSdnXml(sdnText.replace('<Record_Count>4</Record_Count>', '<Record_Count>5</Record_Count>')), /4 entries but Record_Count 5/);
  const cut = sdnText.slice(0, sdnText.lastIndexOf('<sdnEntry>'));
  assert.throws(() => parseSdnXml(cut), XmlError);
});

test('advanced: a digital-currency feature of an undeclared type, or with two details, is refused', () => {
  assert.throws(() => parseSdnAdvancedXml(advText.replace(/<FeatureType ID="345"[^>]*>Digital Currency Address - ETH<\/FeatureType>/, '')), /not declared before use/);
  const two = advText.replace(/(<VersionDetail DetailTypeID="\d+">0x19F8f2B0915Daa12a3f5C9CF01dF9E24D53794F7<\/VersionDetail>)/, '$1$1');
  assert.notEqual(two, advText);
  assert.throws(() => parseSdnAdvancedXml(two), /with 2 details/);
});

test('XML reader refuses DOCTYPE, unknown entities, bare ampersands, mismatched tags, text outside the root', () => {
  assert.throws(() => walk('<!DOCTYPE x [<!ENTITY a "b">]><x>&a;</x>'), /DOCTYPE/);
  assert.throws(() => walk('<x>&foo;</x>'), /unknown entity/);
  assert.throws(() => walk('<x>a & b</x>'), /bare &/);
  assert.throws(() => walk('<x><y></x></y>'), /does not match/);
  assert.throws(() => walk('<x/>junk'), /outside the root/);
  assert.throws(() => walk('<x/><y/>'), /second root/);
  assert.throws(() => walk('<x a="1" a="2"/>'), /duplicate attribute/);
  let t = ''; walk('<x>a &amp; &#x41;&#66; <![CDATA[<raw>]]></x>', { onText: (s) => { t += s; } });
  assert.equal(t, 'a & AB <raw>');
});

test('an empty publication is refused', () => {
  const none = (s) => s.replace(/<id>\s*<uid>\d+<\/uid>\s*<idType>Digital Currency Address - [A-Z]+<\/idType>\s*<idNumber>[^<]+<\/idNumber>\s*<\/id>/g, '');
  const advNone = advText.replace(/<Feature ID="\d+" FeatureTypeID="(344|345|566|887|992|998)">[\s\S]*?<\/Feature>/g, '');
  assert.throws(() => build({ sdn: none(sdnText), adv: advNone }), /refusing an empty list/);
});

test('a removal against the previous build needs explicit confirmation; an older publication is refused', () => {
  const prev = build();
  prev.evm.push({ address: '0xffffffffffffffffffffffffffffffffffffffff', symbols: ['ETH'], uids: ['1'] });
  assert.throws(() => build({ extra: { previous: prev } }), /absent \(delisting\?\)/);
  const ok = build({ extra: { previous: prev, acceptRemovals: ['0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF'] } });
  assert.deepEqual(ok.removalsAccepted, ['0xffffffffffffffffffffffffffffffffffffffff']);
  const newer = build(); newer.source.publishDate = '2026-12-01';
  assert.throws(() => build({ extra: { previous: newer } }), /older than the previous build/);
});

test('loadSanctions refuses hand-edited, reordered or uppercase lists and checks freshness', () => {
  const j = readJson('fixtures/sanctions-fixture.json');
  const s = loadSanctions(j);
  assert.equal(s.size, 3);
  assert.equal(s.has('0x983A81CA6FB1E441266D2FBCB7D8E530AC2E05A2'), true);
  assert.equal(s.has('0x00000000000000000000000000000000000000a1'), false);
  const b = Date.parse('2026-11-17T00:00:00Z');
  assert.equal(s.fresh(b), true); assert.equal(s.fresh(b + 48 * 3600e3), true);
  assert.equal(s.fresh(b + 48 * 3600e3 + 1), false); assert.equal(s.fresh(b - 1), false);
  const drop = structuredClone(j); drop.evm.pop();
  assert.throws(() => loadSanctions(drop), /content hash mismatch/);
  const drop2 = structuredClone(j); drop2.evm.pop(); drop2.evmAddressesSha256 = evmAddressesSha256(drop2.evm.map((e) => e.address));
  assert.doesNotThrow(() => loadSanctions(drop2), 'a consistent smaller list loads: the builder, not the loader, guards removals');
  const rev = structuredClone(j); rev.evm.reverse();
  assert.throws(() => loadSanctions(rev), /not strictly sorted/);
  const up = structuredClone(j); up.evm[0].address = up.evm[0].address.toUpperCase().replace('0X', '0x');
  assert.throws(() => loadSanctions(up), /lowercase/);
  const dup = structuredClone(j); dup.evm.splice(1, 0, dup.evm[0]);
  assert.throws(() => loadSanctions(dup), /not strictly sorted/);
  assert.throws(() => loadSanctions({ ...j, schema: 'x' }), SanctionsError);
  assert.throws(() => loadSanctions({ ...j, builtUtc: 'yesterday' }), /builtUtc/);
});

test('the committed full build (data/sanctions-evm.json) loads and agrees with its own source record', () => {
  const j = readJson('data/sanctions-evm.json');
  const s = loadSanctions(j);
  assert.equal(s.size, j.counts.evmAddresses);
  assert.ok(s.size >= 100, 'the 2026-11-13 publication lists 127 EVM-shaped addresses');
  assert.equal(j.source.files.length, 2);
  assert.match(j.source.crossCheck, /name the same \d+ \(uid, currency, address\) triples/);
  assert.equal(j.counts.triples, Object.values(j.counts.byType).reduce((a, b) => a + b, 0));
  assert.equal(j.counts.duplicatesWithinSdnXml, 1, 'uid 37070 lists one XBT address twice in the 2026-11-13 publication');
});
