#!/usr/bin/env node
// Bundle budget: node scripts/check-bundle-size.mjs <dist> [--json FILE]
// Fails (exit 1) when the built JavaScript or CSS is larger than the stated budget, gzip (level 9, node:zlib).
// The budget is a decision, not a measurement: it is where the bundle stands after design pass 3 plus about 10%, so
// growth is noticed and argued for, never silent.
//
// Since design pass 3 the build is split by route. Two JavaScript figures are budgeted:
//   entryGzip  what the FIRST page (home) loads before anything is clicked: the entry script named in index.html and
//              every chunk it imports statically, transitively. The terminal (chart, book, order form) and the other
//              pages are not in it.
//   jsGzip     every chunk together (what a visitor has fetched after opening every page).
import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { gzipSync } from 'node:zlib';

export const BUDGET = { entryGzip: 158_000, jsGzip: 225_000, cssGzip: 18_000, fonts: 120_000, total: 585_000 };
const dist = process.argv[2];
if (!dist) {
  console.error('usage: check-bundle-size.mjs <dist> [--json FILE]');
  process.exit(2);
}
const jsonAt = process.argv.indexOf('--json');
const walk = (dir) => readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(join(dir, e.name)) : [join(dir, e.name)]));
const files = walk(dist);
const gz = (f) => gzipSync(readFileSync(f), { level: 9 }).length;
const size = (pattern, zipped) => files.filter((f) => pattern.test(f)).reduce((s, f) => s + (zipped ? gz(f) : readFileSync(f).length), 0);

// The entry: the module script of index.html and its static imports (import … from "./x.js", import "./x.js",
// export … from "./x.js"); a dynamic import("./x.js") is a route's chunk and is not followed.
const html = readFileSync(join(dist, 'index.html'), 'utf8');
const entry = /<script type="module"[^>]*\ssrc="([^"]+)"/.exec(html)?.[1];
if (!entry) {
  console.error('no module script in index.html');
  process.exit(2);
}
const seen = new Set();
const follow = (file) => {
  if (seen.has(file)) return;
  seen.add(file);
  const code = readFileSync(file, 'utf8');
  for (const m of code.matchAll(/(?:^|[;}\s])(?:import|export)\s*(?:[^"'()]*?from\s*)?["'](\.{1,2}\/[^"']+\.js)["']/g)) follow(resolve(dirname(file), m[1]));
};
follow(resolve(dist, entry));
const routeChunks = files.filter((f) => f.endsWith('.js') && !seen.has(resolve(f)) && dirname(f).endsWith('assets')).map((f) => ({ file: f.slice(dist.length + 1), gzip: gz(f) }));

const measured = {
  entryGzip: [...seen].reduce((s, f) => s + gz(f), 0),
  jsGzip: size(/\.js$/, true),
  cssGzip: size(/\.css$/, true),
  fonts: size(/\.woff2$/, false),
  total: size(/\.(js|css|html|svg|webmanifest)$/, true) + size(/\.(woff2|png)$/, false),
};
let failed = 0;
for (const [key, limit] of Object.entries(BUDGET)) {
  const over = measured[key] > limit;
  if (over) failed++;
  console.log(`${over ? 'OVER ' : 'ok   '} ${key.padEnd(9)} ${String(measured[key]).padStart(7)} B of ${limit} B`);
}
console.log(`entry: ${[...seen].length} file(s); ${routeChunks.length} chunk(s) loaded on demand`);
if (jsonAt > 0) writeFileSync(process.argv[jsonAt + 1], JSON.stringify({ budget: BUDGET, measured, entryFiles: [...seen].map((f) => ({ file: f.slice(resolve(dist).length + 1), gzip: gz(f) })), onDemand: routeChunks }, null, 1) + '\n');
process.exit(failed ? 1 : 0);
