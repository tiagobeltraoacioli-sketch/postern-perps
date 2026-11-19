#!/usr/bin/env node
// Measurements of a built page with the installed Chrome over CDP. UNAUDITED prototype, loopback only.
//
//   node integration/browser/measure.mjs --app http://127.0.0.1:PORT/ --out FILE.json [--axe path/to/axe.min.js]
//        [--mock http://127.0.0.1:49810] [--port 49856] [--busy]
//
// What it records (each number with its method, in the output file):
//  - accessibility: axe-core rules (WCAG 2.0/2.1 A and AA + best practices) on each route in both themes; the tool
//    is NOT a dependency of the project: pass the path of axe.min.js (the report names package, version, licence).
//  - load: navigation timing, first contentful paint, largest contentful paint, layout shift, total blocking time
//    (long tasks over 50 ms after FCP, 5 s window), transferred bytes of the app's own files. "Lighthouse-style"
//    metrics measured directly; Lighthouse itself is not installed and no score is invented.
//  - frames: with --busy the mock's busy scenario (>= 300 WebSocket messages per second) runs for 10 s on the
//    trading screen: frame intervals (requestAnimationFrame), long tasks, the feed's own flush times and the
//    chart's per-frame work (window.__perps, local render builds only).
import { readFileSync, writeFileSync } from 'node:fs';
import { setTimeout as delay } from 'node:timers/promises';
import { launch, openPage } from './cdp.mjs';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const APP = opt('app', 'http://127.0.0.1:49811/');
const OUT = opt('out', './measure.json');
const AXE = opt('axe', '');
const MOCK = opt('mock', '');
const PORT = Number(opt('port', '49856'));
const BUSY = args.includes('--busy');
const ROUTES = opt('routes', '#/,#/trade/BTC-USD,#/markets,#/portfolio,#/transfers/deposit,#/transfers/withdraw,#/transfers/exit,#/history/fills,#/funding/BTC-USD,#/agents,#/system,#/legal,#/nowhere').split(',');
const CHROME = opt('chrome', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');
const axeSource = AXE ? readFileSync(AXE, 'utf8') : '';

const initScript = String.raw`
(() => {
  const address = '0x1111111111111111111111111111111111111111';
  const provider = { request: async ({ method }) => { if (method === 'eth_requestAccounts' || method === 'eth_accounts') return [address]; if (method === 'eth_chainId') return '0xab162'; throw Object.assign(new Error('watch-only'), { code: 4001 }); }, on() {}, removeListener() {} };
  const info = { uuid: 'ad506f7e-8192-4da4-9ebf-304b5c6d7e8f', name: 'Watch-only wallet', icon: '', rdns: 'local.watch' };
  const announce = () => window.dispatchEvent(new CustomEvent('eip6963:announceProvider', { detail: Object.freeze({ info, provider }) }));
  window.addEventListener('eip6963:requestProvider', announce);
  announce();
  // Performance observers installed before the page's own scripts.
  window.__m = { lcp: 0, cls: 0, long: [], fcp: 0 };
  try {
    new PerformanceObserver((l) => { for (const e of l.getEntries()) window.__m.lcp = e.startTime; }).observe({ type: 'largest-contentful-paint', buffered: true });
    new PerformanceObserver((l) => { for (const e of l.getEntries()) if (!e.hadRecentInput) window.__m.cls += e.value; }).observe({ type: 'layout-shift', buffered: true });
    new PerformanceObserver((l) => { for (const e of l.getEntries()) window.__m.long.push([e.startTime, e.duration]); }).observe({ type: 'longtask', buffered: true });
    new PerformanceObserver((l) => { for (const e of l.getEntries()) if (e.name === 'first-contentful-paint') window.__m.fcp = e.startTime; }).observe({ type: 'paint', buffered: true });
  } catch {}
})();`;

const eligibility = readFileSync(new URL('../../src/compliance/eligibility.ts', import.meta.url), 'utf8');
const constant = (name) => new RegExp(`${name} = '([^']+)'`).exec(eligibility)[1];
const ATTESTATION_KEY = constant('KEY');
const ATTESTATION = JSON.stringify([{ message: { account: '0x1111111111111111111111111111111111111111', statementId: constant('STATEMENT_ID'), statementHash: constant('STATEMENT_HASH'), residenceCountry: 'PT', issuedAt: String(Date.now()), expiresAt: String(Date.now() + 86_400_000) }, signature: '0x' }]);

const quantile = (list, q) => {
  if (!list.length) return null;
  const s = [...list].sort((a, b) => a - b);
  return Math.round(s[Math.min(s.length - 1, Math.floor(q * s.length))] * 100) / 100;
};
const browser = await launch(CHROME, PORT);
const result = { app: APP, chrome: browser.product, at: new Date().toISOString(), method: 'integration/browser/measure.mjs', accessibility: null, load: [], frames: null };
try {
  const { browserContextId } = await browser.send('Target.createBrowserContext');
  const page = await openPage(browser, browserContextId, { initScript, url: 'about:blank', onBinding: async () => {}, log: () => {} });
  await page.cdp('Emulation.setDeviceMetricsOverride', { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
  await page.cdp('Network.enable');
  const connect = async () => {
    if (await page.evaluate(`!!window.__h.button('Connect')`)) {
      await page.click('Connect');
      await page.click('Watch-only wallet').catch(() => {});
    }
  };

  // ---- load metrics: cold cache, home and terminal
  for (const route of ['#/', '#/trade/BTC-USD']) {
    await page.cdp('Network.clearBrowserCache');
    await page.goto('about:blank');
    await page.goto(`${APP}${route}`);
    await delay(6000);
    const m = JSON.parse(
      await page.evaluate(`JSON.stringify((() => {
        const nav = performance.getEntriesByType('navigation')[0];
        const own = performance.getEntriesByType('resource').filter((r) => r.name.startsWith(location.origin));
        const fcp = window.__m.fcp;
        const tbt = window.__m.long.filter(([t]) => t >= fcp && t < fcp + 5000).reduce((s, [, d]) => s + Math.max(0, d - 50), 0);
        return { domContentLoaded: nav.domContentLoadedEventEnd, load: nav.loadEventEnd, fcp, lcp: window.__m.lcp, cls: window.__m.cls, tbt, longTasks: window.__m.long.length,
          transferred: own.reduce((s, r) => s + (r.transferSize || 0), 0) + (nav.transferSize || 0), decoded: own.reduce((s, r) => s + (r.decodedBodySize || 0), 0) + (nav.decodedBodySize || 0), requests: own.length + 1 };
      })())`),
    );
    for (const k of Object.keys(m)) m[k] = Math.round(m[k] * 1000) / 1000;
    result.load.push({ route, viewport: '1440x900', cache: 'cold', ...m });
  }

  // ---- accessibility
  if (axeSource) {
    const runs = [];
    for (const theme of ['dark', 'light']) {
      await page.goto(APP);
      await page.evaluate(`localStorage.setItem('postern.theme.v1', ${JSON.stringify(theme)})`);
      const check = async (route) => {
        await page.evaluate(axeSource);
        const r = JSON.parse(
          await page.evaluate(`axe.run(document, { runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'best-practice'] }, resultTypes: ['violations', 'incomplete'] }).then((r) => JSON.stringify({ version: axe.version, passes: r.passes.length, violations: r.violations.map((v) => ({ id: v.id, impact: v.impact, help: v.help, nodes: v.nodes.slice(0, 6).map((n) => ({ target: n.target.join(' '), summary: (n.failureSummary || '').slice(0, 220) })), count: v.nodes.length })), incomplete: r.incomplete.map((v) => ({ id: v.id, count: v.nodes.length })) }))`),
        );
        runs.push({ theme, route, ...r });
      };
      for (const route of ROUTES) {
        await page.evaluate(`localStorage.removeItem(${JSON.stringify(ATTESTATION_KEY)})`);
        await page.goto(`${APP}${route}`);
        await page.reload();
        await connect();
        await delay(2500);
        await check(route);
        if (!route.startsWith('#/trade')) continue;
        // The terminal again with the order form drawn (a display-only attestation record: nothing can be signed with
        // it) and then with the depth view and the moving averages on.
        await page.evaluate(`localStorage.setItem(${JSON.stringify(ATTESTATION_KEY)}, ${JSON.stringify(ATTESTATION)})`);
        await page.reload();
        await connect();
        await delay(2500);
        await check(`${route} (order form)`);
        await page.evaluate(`(() => { document.querySelector('.side-tabs [data-tab=depth]')?.click(); const m = document.querySelector('.pc-ma-toggle'); if (m && m.getAttribute('aria-pressed') !== 'true') m.click(); })()`);
        await delay(800);
        await check(`${route} (depth view, averages on)`);
        await page.evaluate(`(() => { const m = document.querySelector('.pc-ma-toggle'); if (m && m.getAttribute('aria-pressed') === 'true') m.click(); })()`);
      }
    }
    result.accessibility = { tool: 'axe-core', version: runs[0]?.version, licence: 'MPL-2.0 (run from a scratch copy; not a dependency, not bundled)', rules: 'wcag2a, wcag2aa, wcag21a, wcag21aa, best-practice', pages: runs.length, violations: runs.reduce((s, r) => s + r.violations.reduce((t, v) => t + v.count, 0), 0), runs };
  }

  // ---- frames under the busy feed
  if (BUSY && MOCK) {
    await page.goto(`${APP}#/trade/BTC-USD`);
    await page.reload();
    await connect();
    await delay(3000);
    await fetch(`${MOCK}/mock/scenario?busy=1`);
    await delay(1500);
    const frames = JSON.parse(
      await page.evaluate(`new Promise((done) => {
        const gaps = []; let last = performance.now(); const t0 = last; const long0 = window.__m.long.length;
        const cs = window.__perps?.chartStats; if (cs) cs.frameMs.length = 0; const fs = window.__perps?.feedStats; const m0 = fs ? fs.messages : 0; const f0 = fs ? fs.flushes : 0; if (fs) fs.flushMs.length = 0;
        const tick = (t) => { gaps.push(t - last); last = t; if (t - t0 < 10000) requestAnimationFrame(tick); else done(JSON.stringify({ gaps, long: window.__m.long.slice(long0).map(([, d]) => d), messages: fs ? fs.messages - m0 : null, flushes: fs ? fs.flushes - f0 : null, flushMs: fs ? fs.flushMs.slice() : [], chartMs: cs ? cs.frameMs.slice() : [], bookRows: document.querySelectorAll('.book-row').length })); };
        requestAnimationFrame(tick);
      })`),
    );
    await fetch(`${MOCK}/mock/scenario?busy=0`);
    result.frames = {
      scenario: 'node/mock busy=1 for 10 s, trading screen 1440x900, headless Chrome; the build is the one served at --app',
      messagesPerSecond: frames.messages === null ? null : Math.round(frames.messages / 10),
      frames: frames.gaps.length,
      frameIntervalMs: { p50: quantile(frames.gaps, 0.5), p95: quantile(frames.gaps, 0.95), p99: quantile(frames.gaps, 0.99), max: quantile(frames.gaps, 1) },
      over33ms: frames.gaps.filter((g) => g > 33.4).length,
      longTasks: { count: frames.long.length, maxMs: quantile(frames.long, 1) },
      feedFlushMs: { count: frames.flushMs.length, p50: quantile(frames.flushMs, 0.5), p95: quantile(frames.flushMs, 0.95), max: quantile(frames.flushMs, 1) },
      chartFrameWorkMs: { count: frames.chartMs.length, p50: quantile(frames.chartMs, 0.5), p95: quantile(frames.chartMs, 0.95), p99: quantile(frames.chartMs, 0.99), max: quantile(frames.chartMs, 1) },
      bookRowsOnScreen: frames.bookRows,
    };
  }
} finally {
  await browser.close();
}
writeFileSync(OUT, `${JSON.stringify(result, null, 1)}\n`);
const a = result.accessibility;
console.log(`load: ${result.load.map((l) => `${l.route} FCP ${l.fcp} ms LCP ${l.lcp} ms CLS ${l.cls} TBT ${l.tbt} ms ${l.transferred} B`).join(' | ')}`);
if (a) console.log(`axe-core ${a.version}: ${a.violations} violation node(s) on ${a.pages} page runs`);
if (a) for (const r of a.runs) for (const v of r.violations) console.log(`  ${r.theme} ${r.route}: ${v.id} (${v.impact}) x${v.count}: ${v.nodes[0]?.target}`);
if (result.frames) console.log(`frames: ${JSON.stringify(result.frames.frameIntervalMs)} over33=${result.frames.over33ms} of ${result.frames.frames} msg/s=${result.frames.messagesPerSecond} chart ${JSON.stringify(result.frames.chartFrameWorkMs)} flush ${JSON.stringify(result.frames.feedFlushMs)} long ${JSON.stringify(result.frames.longTasks)}`);
