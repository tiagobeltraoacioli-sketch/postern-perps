#!/usr/bin/env node
// Bundle budget: node scripts/check-bundle-size.mjs <dist>
// Fails (exit 1) when the built JavaScript or CSS is larger than the stated budget, gzip (level 9, node:zlib).
// The budget is a decision, not a measurement: it is where the bundle stands after design pass 2 plus about 10%, so
// growth is noticed and argued for, never silent. One chunk today (no route splitting): see the report.
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';

export const BUDGET = { jsGzip: 210_000, cssGzip: 16_500, fonts: 120_000, total: 560_000 };
const dist = process.argv[2];
if (!dist) {
  console.error('usage: check-bundle-size.mjs <dist>');
  process.exit(2);
}
const walk = (dir) => readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(join(dir, e.name)) : [join(dir, e.name)]));
const files = walk(dist);
const size = (pattern, gz) => files.filter((f) => pattern.test(f)).reduce((s, f) => s + (gz ? gzipSync(readFileSync(f), { level: 9 }).length : readFileSync(f).length), 0);
const measured = {
  jsGzip: size(/\.js$/, true),
  cssGzip: size(/\.css$/, true),
  fonts: size(/\.woff2$/, false),
  total: size(/\.(js|css|html|svg|webmanifest)$/, true) + size(/\.(woff2|png)$/, false),
};
let failed = 0;
for (const [key, limit] of Object.entries(BUDGET)) {
  const over = measured[key] > limit;
  if (over) failed++;
  console.log(`${over ? 'OVER ' : 'ok   '} ${key.padEnd(8)} ${String(measured[key]).padStart(7)} B of ${limit} B`);
}
process.exit(failed ? 1 : 0);
