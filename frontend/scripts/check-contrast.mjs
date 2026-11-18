#!/usr/bin/env node
// WCAG 2.1 contrast check of the design tokens: node scripts/check-contrast.mjs
// Reads src/styles/tokens.css and fails (exit 1) when a text pair is below 4.5:1
// or an essential non-text pair (control borders, the network lamp, focus) is below 3:1.
import { readFileSync } from 'node:fs';

const css = readFileSync(new URL('../src/styles/tokens.css', import.meta.url), 'utf8');
const block = (selector) => {
  const at = css.indexOf(selector + ' {');
  if (at < 0) throw new Error(`no block ${selector}`);
  const body = css.slice(css.indexOf('{', at) + 1, css.indexOf('}', at));
  return Object.fromEntries([...body.matchAll(/(--[a-z0-9-]+):\s*(#[0-9a-f]{6})\s*;/g)].map((m) => [m[1], m[2]]));
};
const lum = (hex) => {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};
const ratio = (a, b) => {
  const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
};

const NETWORKS = ['bloch-l2', 'ethereum', 'linea', 'arbitrum-one', 'bnb-smart-chain', 'base', 'op-mainnet', 'devnet4', 'preview'];
const themes = {
  dark: { tokens: block(":root[data-theme='dark']"), net: (n) => block(`\n[data-network='${n}']`)['--net-bright'] },
  light: { tokens: block(":root[data-theme='light']"), net: (n) => block(`:root[data-theme='light'] [data-network='${n}']`)['--net'] },
};

let failed = 0;
let checked = 0;
const check = (theme, label, fg, bg, min) => {
  const r = ratio(fg, bg);
  checked++;
  if (r < min) {
    failed++;
    console.log(`FAIL ${theme} ${label}: ${r.toFixed(2)} < ${min} (${fg} on ${bg})`);
  }
};
for (const [name, { tokens: t, net }] of Object.entries(themes)) {
  const surfaces = ['--bg', '--surface', '--surface-2', '--surface-3'];
  for (const s of surfaces) {
    for (const fg of ['--text', '--text-2', '--text-3', '--ok', '--warn', '--danger', '--info']) check(name, `${fg} on ${s}`, t[fg], t[s], 4.5);
    check(name, `--focus on ${s}`, t['--focus'], t[s], 3);
  }
  for (const s of ['--bg', '--surface', '--surface-2']) {
    check(name, `--control-border on ${s}`, t['--control-border'], t[s], 3);
    for (const n of NETWORKS) check(name, `lamp ${n} on ${s}`, net(n), t[s], 3);
  }
  for (const n of NETWORKS) check(name, `primary action label on the ${n} lamp`, t['--accent-fg'], net(n), 4.5);
  check(name, 'primary button label', t['--primary-fg'], t['--primary-bg'], 4.5);
  check(name, 'primary button label (hover)', t['--primary-fg'], t['--primary-bg-hover'], 4.5);
  check(name, 'primary button against the page', t['--primary-bg'], t['--surface'], 3);
}
// Brand mark and network tiles: the bright lamp and the tile letter on the dark tile, in both themes.
for (const n of NETWORKS) check('tile', `lamp ${n} on the tile`, themes.dark.net(n), '#14191a', 3);
check('tile', 'tile letter', '#eceae3', '#14191a', 4.5);
// Network glyphs (marks/NetworkGlyph.tsx): the ink monogram on the gate filled with the network's bright lamp,
// in both themes; and the badges of the networks a bridged token comes from.
const ORIGINS = ['bitcoin', 'solana', 'tron', 'stellar'];
for (const n of [...NETWORKS, ...ORIGINS]) check('glyph', `monogram on the ${n} gate`, '#0b0e0f', themes.dark.net(n), 4.5);
console.log(`${checked} pairs checked, ${failed} below WCAG AA`);
process.exit(failed ? 1 : 0);
