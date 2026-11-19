#!/usr/bin/env node
// Renders the site's raster brand assets from drawings in this file with the installed Chrome (headless, no network):
//   public/icons/icon-192.png, icon-512.png, icon-maskable-512.png, apple-touch-icon.png (180), public/social.png (1200x630)
// Everything is drawn here (the Postern mark: wall, gate, lamp; Fraunces from the self-hosted font file): no
// third-party asset. Run after changing the mark:  node scripts/make-brand-assets.mjs [--port 49855]
// UNAUDITED prototype; the files are committed, this script is how they were made.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { launch, openPage } from '../integration/browser/cdp.mjs';

const frontend = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const port = Number(process.argv[process.argv.indexOf('--port') + 1] || 49855) || 49855;
const fraunces = readFileSync(resolve(frontend, 'node_modules/@fontsource-variable/fraunces/files/fraunces-latin-opsz-normal.woff2')).toString('base64');
const inter = readFileSync(resolve(frontend, 'node_modules/@fontsource-variable/inter/files/inter-latin-wght-normal.woff2')).toString('base64');
const BG = '#0b0e0f';
const TILE = '#14191a';
const EDGE = '#364145';
const FG = '#eceae3';
const LAMP = '#c8fa72';

/** The mark on a square: `pad` is the share of the side left empty around the tile (maskable icons need more). */
const mark = (size, pad, rounded) => `
<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 32 32">
  <rect width="32" height="32" fill="${rounded ? 'none' : TILE}"/>
  <g transform="translate(${16 * pad} ${16 * pad}) scale(${1 - pad})">
    <rect x="0.5" y="0.5" width="31" height="31" rx="${rounded ? 6.5 : 0}" fill="${TILE}" stroke="${rounded ? EDGE : 'none'}"/>
    <path d="M10.5 26V16.5a5.5 5.5 0 0 1 11 0V26Z" fill="${FG}"/>
    <rect x="6" y="26" width="20" height="1.5" rx="0.75" fill="${FG}" opacity="0.55"/>
    <circle cx="16" cy="6.75" r="1.75" fill="${LAMP}"/>
  </g>
</svg>`;

const social = `
<div style="width:1200px;height:630px;background:${BG};display:flex;align-items:center;justify-content:space-between;padding:0 84px;box-sizing:border-box;position:relative;overflow:hidden">
  <svg width="1200" height="630" style="position:absolute;inset:0" xmlns="http://www.w3.org/2000/svg">
    <defs><pattern id="m" width="176" height="96" patternUnits="userSpaceOnUse"><path d="M0 .5h176M0 48.5h176M.5 0v48M88.5 48v48" fill="none" stroke="#8a9492" stroke-opacity=".13"/></pattern>
    <radialGradient id="g" cx="78%" cy="24%" r="46%"><stop offset="0" stop-color="${LAMP}" stop-opacity=".22"/><stop offset="1" stop-color="${LAMP}" stop-opacity="0"/></radialGradient></defs>
    <rect width="1200" height="630" fill="url(#m)"/><rect width="1200" height="630" fill="url(#g)"/>
  </svg>
  <div style="position:relative;max-width:700px">
    <div style="font:500 22px Inter;letter-spacing:.14em;color:#a8b0ae">POSTERNDEX · PERPETUAL FUTURES</div>
    <div style="font:560 92px/1.02 Fraunces;letter-spacing:-.028em;color:${FG};margin-top:26px">Trade in the open.<br/>Keep the way out.</div>
    <div style="font:450 26px/1.45 Inter;color:#a8b0ae;margin-top:30px">One transparent order book. Collateral in a vault with no admin key.</div>
    <div style="font:450 20px Inter;color:#8a9492;margin-top:34px">By Bloch Inc · unaudited prototype, not live</div>
  </div>
  <svg width="300" height="380" viewBox="0 0 300 380" style="position:relative" xmlns="http://www.w3.org/2000/svg">
    <rect x="10" y="10" width="280" height="350" rx="6" fill="#232a2d" stroke="#3b474b"/>
    <path d="M78 360V190a72 72 0 0 1 144 0V360" fill="none" stroke="#3b474b" stroke-width="24"/>
    <path d="M78 360V190a72 72 0 0 1 144 0V360" fill="none" stroke="#2a3235" stroke-width="20"/>
    <path d="M88 360V190a62 62 0 0 1 124 0V360Z" fill="${BG}"/>
    <path d="M108 360 150 250 192 360Z" fill="${LAMP}" opacity=".09"/>
    <rect x="0" y="358" width="300" height="10" rx="3" fill="#1d2426" stroke="#3b474b"/>
    <path d="M150 10v44" stroke="#6b7b81" stroke-width="2"/><path d="M139 54h22l-4 24h-14Z" fill="${BG}" stroke="#6b7b81" stroke-width="2"/>
    <circle cx="150" cy="65" r="6" fill="${LAMP}"/>
  </svg>
</div>`;

const html = (body, w, h) => `<!doctype html><html><head><meta charset="utf-8"><style>
@font-face{font-family:Fraunces;src:url(data:font/woff2;base64,${fraunces}) format('woff2');font-weight:100 900}
@font-face{font-family:Inter;src:url(data:font/woff2;base64,${inter}) format('woff2');font-weight:100 900}
html,body{margin:0;width:${w}px;height:${h}px;background:transparent;overflow:hidden}</style></head><body>${body}</body></html>`;

const jobs = [
  ['icons/icon-192.png', mark(192, 0, true), 192, 192, true],
  ['icons/icon-512.png', mark(512, 0, true), 512, 512, true],
  ['icons/icon-maskable-512.png', mark(512, 0.2, false), 512, 512, false],
  ['icons/apple-touch-icon.png', mark(180, 0.08, false), 180, 180, false],
  ['social.png', social, 1200, 630, false],
];
mkdirSync(resolve(frontend, 'public/icons'), { recursive: true });
const browser = await launch('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', port);
try {
  const { browserContextId } = await browser.send('Target.createBrowserContext');
  const page = await openPage(browser, browserContextId, { initScript: '', url: 'about:blank', onBinding: async () => {}, log: () => {} });
  for (const [file, body, w, h, transparent] of jobs) {
    await page.cdp('Emulation.setDeviceMetricsOverride', { width: w, height: h, deviceScaleFactor: 1, mobile: false });
    await page.cdp('Emulation.setDefaultBackgroundColorOverride', transparent ? { color: { r: 0, g: 0, b: 0, a: 0 } } : {});
    await page.goto(`data:text/html;base64,${Buffer.from(html(body, w, h)).toString('base64')}`);
    await page.evaluate('document.fonts.ready.then(() => true)');
    await delay(300);
    const { data } = await page.cdp('Page.captureScreenshot', { format: 'png', clip: { x: 0, y: 0, width: w, height: h, scale: 1 } });
    writeFileSync(resolve(frontend, 'public', file), Buffer.from(data, 'base64'));
    console.log(`${file} ${w}x${h} ${Buffer.from(data, 'base64').length} bytes`);
  }
} finally {
  await browser.close();
}
