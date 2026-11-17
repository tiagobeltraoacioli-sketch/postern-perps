#!/usr/bin/env node
// Dev tool: cut a small, internally consistent excerpt of the two OFAC files for test fixtures.
// node tools/make-sdn-excerpt.mjs <SDN.XML> <SDN_ADVANCED.XML> <outdir> <uid>...
// The excerpt keeps the publication header and date, the chosen entries verbatim, the advanced file's
// FeatureTypeValues block verbatim, and the chosen DistinctParty blocks verbatim. Record_Count is set to the
// number of entries kept (the only edit; stated in fixtures/README.md). Public U.S. Government data.

import { readFileSync, writeFileSync } from 'node:fs';

const [sdnPath, advPath, out, ...uids] = process.argv.slice(2);
if (!out || uids.length === 0) { console.error('usage: make-sdn-excerpt.mjs SDN.XML SDN_ADVANCED.XML outdir uid...'); process.exit(2); }
const sdn = readFileSync(sdnPath, 'utf8');
const adv = readFileSync(advPath, 'utf8');

const head = sdn.slice(0, sdn.indexOf('<sdnEntry>'));
const entries = [...sdn.matchAll(/  <sdnEntry>\r?\n    <uid>(\d+)<\/uid>[\s\S]*?<\/sdnEntry>\r?\n/g)].filter((m) => uids.includes(m[1]));
if (entries.length !== uids.length) throw new Error(`found ${entries.length} of ${uids.length} SDN entries`);
const sdnOut = head.replace(/<Record_Count>\d+<\/Record_Count>/, `<Record_Count>${entries.length}</Record_Count>`)
  + entries.map((m) => m[0]).join('') + '</sdnList>\n';

const advHead = adv.slice(0, adv.indexOf('<ReferenceValueSets>'));
const ftv = /<FeatureTypeValues>[\s\S]*?<\/FeatureTypeValues>/.exec(adv)[0];
const parties = [...adv.matchAll(/<DistinctParty FixedRef="(\d+)"[\s\S]*?<\/DistinctParty>/g)].filter((m) => uids.includes(m[1]));
if (parties.length !== uids.length) throw new Error(`found ${parties.length} of ${uids.length} distinct parties`);
const advOut = advHead + '<ReferenceValueSets>\n    ' + ftv + '\n  </ReferenceValueSets>\n  <DistinctParties>\n    '
  + parties.map((m) => m[0]).join('\n    ') + '\n  </DistinctParties>\n</Sanctions>\n';

writeFileSync(`${out}/sdn-excerpt.xml`, sdnOut);
writeFileSync(`${out}/sdn-advanced-excerpt.xml`, advOut);
console.log(`wrote ${entries.length} entries`);
