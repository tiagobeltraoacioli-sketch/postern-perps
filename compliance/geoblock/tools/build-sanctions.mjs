#!/usr/bin/env node
// Build sanctions-evm.json from the two official OFAC files already downloaded (the download is a separate,
// logged step: see DESIGN.md "Sanctions list update procedure"). UNAUDITED prototype.
//
// node tools/build-sanctions.mjs --sdn SDN.XML --advanced SDN_ADVANCED.XML --out sanctions-evm.json
//      [--previous old.json] [--accept-removal 0x...]... [--built-utc 2026-11-17T04:00:00Z]
// Exit 0 and writes --out only when both formats agree and every check passes; exit 1 otherwise (nothing written).

import { readFileSync, writeFileSync, renameSync } from 'node:fs';
import { parseSdnXml, parseSdnAdvancedXml, buildSanctions } from '../src/sanctions.mjs';

const URLS = {
  sdn: 'https://sanctionslistservice.ofac.treas.gov/api/PublicationPreview/exports/SDN.XML',
  advanced: 'https://sanctionslistservice.ofac.treas.gov/api/PublicationPreview/exports/SDN_ADVANCED.XML',
};

function args(argv) {
  const o = { acceptRemovals: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]; const v = argv[i + 1];
    if (a === '--sdn') { o.sdn = v; i++; } else if (a === '--advanced') { o.advanced = v; i++; }
    else if (a === '--out') { o.out = v; i++; } else if (a === '--previous') { o.previous = v; i++; }
    else if (a === '--accept-removal') { o.acceptRemovals.push(v); i++; } else if (a === '--built-utc') { o.builtUtc = v; i++; }
    else throw new Error(`unknown argument ${a}`);
  }
  if (!o.sdn || !o.advanced || !o.out) throw new Error('--sdn, --advanced and --out are required');
  return o;
}

try {
  const o = args(process.argv.slice(2));
  const sdnBytes = readFileSync(o.sdn); const advBytes = readFileSync(o.advanced);
  const dec = new TextDecoder('utf-8', { fatal: true });
  const sdn = parseSdnXml(dec.decode(sdnBytes));
  const advanced = parseSdnAdvancedXml(dec.decode(advBytes));
  const previous = o.previous ? JSON.parse(readFileSync(o.previous, 'utf8')) : null;
  const builtUtc = o.builtUtc ?? new Date().toISOString().replace(/\.\d{3}Z$/, 'Z');
  const out = buildSanctions({
    sdn, advanced, builtUtc, previous, acceptRemovals: o.acceptRemovals,
    files: [{ name: 'SDN.XML', url: URLS.sdn, bytes: new Uint8Array(sdnBytes) }, { name: 'SDN_ADVANCED.XML', url: URLS.advanced, bytes: new Uint8Array(advBytes) }],
  });
  const tmp = `${o.out}.tmp-${process.pid}`;
  writeFileSync(tmp, JSON.stringify(out, null, 1) + '\n');
  renameSync(tmp, o.out); // write-then-rename: a reader never sees a half-written list
  console.log(`ok ${out.version}: ${out.counts.triples} listed addresses, ${out.counts.evmAddresses} EVM, ${out.anomalies.length} anomalies, publication ${out.source.publishDate}`);
} catch (e) {
  console.error(`REFUSED: ${e.message}`);
  process.exit(1);
}
