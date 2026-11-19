#!/usr/bin/env node
// Layout acceptance ("nothing clipped, nothing under an action bar"): measures the real page in a real browser at a
// list of viewports and FAILS (exit 1) on any of the defects design pass 3 was asked to remove. Geometry only: no
// signature (the injected wallet is watch-only and declines everything; the attestation record it seeds is for
// display, see shots.mjs). UNAUDITED prototype, loopback only.
//
//   node integration/browser/fit.mjs --app http://127.0.0.1:49811/ [--out FILE.json] [--port 49854]
//        [--sizes "1024x768,1280x720,…"] [--phones "360x740,390x844"]
import { readFileSync, writeFileSync } from 'node:fs';
import { setTimeout as delay } from 'node:timers/promises';
import { launch, openPage } from './cdp.mjs';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const APP = opt('app', 'http://127.0.0.1:49811/');
const OUT = opt('out', '');
const PORT = Number(opt('port', '49854'));
const SIZES = opt('sizes', '1024x768,1280x720,1280x800,1366x768,1440x900,1920x1080,2560x1440').split(',');
const PHONES = opt('phones', '360x740,390x844,768x1024').split(',');
const ACCOUNT = '0x1111111111111111111111111111111111111111';
const CHROME = opt('chrome', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');

const eligibility = readFileSync(new URL('../../src/compliance/eligibility.ts', import.meta.url), 'utf8');
const constant = (name) => new RegExp(`${name} = '([^']+)'`).exec(eligibility)[1];
const attestation = JSON.stringify([
  {
    message: { account: ACCOUNT, statementId: constant('STATEMENT_ID'), statementHash: constant('STATEMENT_HASH'), residenceCountry: 'PT', issuedAt: String(Date.now()), expiresAt: String(Date.now() + 86_400_000) },
    signature: '0x',
  },
]);
const initScript = String.raw`
(() => {
  const address = ${JSON.stringify(ACCOUNT)};
  const provider = {
    request: async ({ method }) => {
      if (method === 'eth_requestAccounts' || method === 'eth_accounts') return [address];
      if (method === 'eth_chainId') return '0xab162';
      throw Object.assign(new Error('watch-only wallet'), { code: 4001 });
    },
    on() {},
    removeListener() {},
  };
  const info = { uuid: '9c4f5e6d-7081-4c93-8dae-2f3a4b5c6d7e', name: 'Watch-only wallet', icon: '', rdns: 'local.watch' };
  const announce = () => window.dispatchEvent(new CustomEvent('eip6963:announceProvider', { detail: Object.freeze({ info, provider }) }));
  window.addEventListener('eip6963:requestProvider', announce);
  announce();
})();`;

const results = [];
let failed = 0;
const expect = (where, ok, what, detail) => {
  results.push({ where, ok: !!ok, what, ...(detail === undefined ? {} : { detail }) });
  if (!ok) {
    failed++;
    console.log(`FAIL ${where}: ${what}${detail === undefined ? '' : ` (${JSON.stringify(detail)})`}`);
  }
};

const browser = await launch(CHROME, PORT);
const { browserContextId } = await browser.send('Target.createBrowserContext');
const page = await openPage(browser, browserContextId, { initScript, url: 'about:blank', onBinding: async () => {}, log: () => {} });

async function open(route, w, h, phone) {
  await page.cdp('Emulation.setDeviceMetricsOverride', { width: w, height: h, deviceScaleFactor: phone ? 2 : 1, mobile: phone });
  await page.goto(APP);
  await page.evaluate(`localStorage.setItem(${JSON.stringify(constant('KEY'))}, ${JSON.stringify(attestation)})`);
  await page.goto(`${APP}${route}`);
  await page.reload();
  if (await page.evaluate(`!!window.__h.button('Connect')`)) {
    await page.click('Connect');
    await page.click('Watch-only wallet').catch(() => {});
  }
  await delay(1800);
}
const noSideScroll = (where) => page.evaluate('document.documentElement.scrollWidth - document.documentElement.clientWidth').then((d) => expect(where, d <= 1, 'the page does not scroll sideways', d));

// What the order gate looks like right now: does its body need scrolling, is anything of the foot outside the panel.
const GATE = `(() => {
  const body = document.querySelector('.gate-body');
  const foot = document.querySelector('.gate-foot');
  const panel = document.querySelector('.order-panel');
  if (!body || !foot || !panel) return null;
  const p = panel.getBoundingClientRect();
  const outside = [...foot.children].filter((c) => { const r = c.getBoundingClientRect(); return r.height > 0 && (r.bottom > p.bottom + 0.5 || r.top < foot.getBoundingClientRect().top - 0.5); }).length;
  const last = body.lastElementChild?.getBoundingClientRect();
  return { hidden: body.scrollHeight - body.clientHeight, outside, lastBottom: last ? Math.round(last.bottom) : null, footTop: Math.round(foot.getBoundingClientRect().top), footBottom: Math.round(foot.getBoundingClientRect().bottom), viewport: innerHeight };
})()`;
const BOOK = `(() => {
  const body = document.querySelector('.book-body');
  if (!body) return null;
  const b = body.getBoundingClientRect();
  const rows = (side) => [...document.querySelectorAll('.book-' + side + ' .book-row')];
  const cut = [...rows('ask'), ...rows('bid')].filter((r) => { const x = r.getBoundingClientRect(); return x.top < b.top - 0.5 || x.bottom > b.bottom + 0.5; }).length;
  const under = [...document.querySelectorAll('.book-row')].filter((r) => { const d = r.querySelector('.book-depth').getBoundingClientRect(); const px = r.querySelector('.book-px').getBoundingClientRect(); return d.width > 0 && d.left < px.right - 0.5; }).length;
  return { asks: rows('ask').length, bids: rows('bid').length, cut, under };
})()`;
const ACCOUNT_TABLE = `(() => {
  const wrap = document.querySelector('.trade-account .table-wrap');
  if (!wrap) return null;
  const w = wrap.getBoundingClientRect();
  const rows = [...wrap.querySelectorAll('tbody tr')];
  const cutRows = rows.filter((r) => { const x = r.getBoundingClientRect(); return x.bottom > w.bottom + 0.5; }).length;
  return { sideways: wrap.scrollWidth - wrap.clientWidth, rows: rows.length, cutRows };
})()`;
const STATS = `(() => {
  const strip = document.querySelector('.market-stats');
  if (!strip) return null;
  const s = strip.getBoundingClientRect();
  return [...strip.querySelectorAll('.stat')].filter((x) => { const r = x.getBoundingClientRect(); return r.width > 0 && r.right > s.right + 0.5; }).length;
})()`;

try {
  for (const size of SIZES) {
    const [w, h] = size.split('x').map(Number);
    // ---- Trade
    await open('#/trade/BTC-USD', w, h, false);
    const where = `trade ${size}`;
    await noSideScroll(where);
    for (const mode of ['Limit', 'Market', 'Stop / TP']) {
      await page.evaluate(`[...document.querySelectorAll('.mode-tablist [role=tab]')].find((b) => b.textContent.trim() === ${JSON.stringify(mode)}).click()`);
      const kinds = mode === 'Stop / TP' ? ['Stop market', 'Stop limit'] : [null];
      for (const kind of kinds) {
        if (kind) await page.evaluate(`(() => { const s = document.querySelector('.gate-body select'); const o = [...s.options].find((x) => x.textContent.trim().toLowerCase() === ${JSON.stringify(kind.toLowerCase())}); if (o) window.__h.set(s, o.value); })()`);
        await delay(250);
        const g = await page.evaluate(GATE);
        const name = kind ?? mode;
        expect(where, g && g.hidden <= 1, `order form (${name}): the body needs no scrolling`, g);
        expect(where, g && g.outside === 0 && g.footBottom <= g.viewport, `order form (${name}): the account figures and the action are whole and on screen`, g);
      }
    }
    await page.evaluate(`[...document.querySelectorAll('.mode-tablist [role=tab]')][0].click()`);
    const book = await page.evaluate(BOOK);
    expect(where, book && book.cut === 0, 'order book: whole rows only', book);
    expect(where, book && book.under === 0, 'order book: no depth bar reaches under a price', book);
    expect(where, book && book.asks >= 5 && book.bids >= 5, 'order book: at least five levels per side', book);
    const table = await page.evaluate(ACCOUNT_TABLE);
    expect(where, table && table.sideways <= 1, 'positions: the table does not scroll sideways', table);
    expect(where, table && (table.rows > 2 || table.cutRows === 0), 'positions: two rows are whole', table);
    expect(where, (await page.evaluate(STATS)) === 0, 'market bar: no figure is cut');
    // The depth view exists and draws both sides.
    await page.evaluate(`document.querySelector('.side-tabs [data-tab=depth]').click()`);
    await delay(400);
    const depth = await page.evaluate(`(() => { const d = document.querySelector('.trade-depth .depth'); if (!d) return null; const r = d.getBoundingClientRect(); return { h: Math.round(r.height), paths: [...d.querySelectorAll('path')].filter((p) => (p.getAttribute('d') || '').length > 10).length }; })()`);
    expect(where, depth && depth.h > 150 && depth.paths === 2, 'depth view: both sides drawn', depth);
    await page.evaluate(`document.querySelector('.side-tabs [data-tab=book]').click()`);

    // ---- Home: the live markets are whole above the fold, or begin under it
    await open('#/', w, h, false);
    const home = await page.evaluate(`(() => { const s = document.querySelector('.strip ul') || document.querySelector('.strip'); const r = s.getBoundingClientRect(); const hero = document.querySelector('.hero').getBoundingClientRect(); return { top: Math.round(r.top), bottom: Math.round(r.bottom), heroBottom: Math.round(hero.bottom), viewport: innerHeight }; })()`);
    expect(`home ${size}`, home.bottom <= home.viewport || home.top >= home.viewport - 1, 'the market strip is whole above the fold or starts under it', home);
    await noSideScroll(`home ${size}`);

    // ---- Markets: the head's figures do not touch the sentence
    await open('#/markets', w, h, false);
    const head = await page.evaluate(`(() => { const s = document.querySelector('.head-stats').getBoundingClientRect(); const l = document.querySelector('.page-head-row .lead').getBoundingClientRect(); const t = document.querySelector('.page-head-row h1').getBoundingClientRect(); const overlap = !(s.right <= l.left || s.left >= l.right || s.bottom <= l.top + 0.5 || s.top >= l.bottom); const wrapped = [...document.querySelectorAll('.market-card-name strong')].filter((x) => x.getBoundingClientRect().height > 30).length; return { overlap, statsBottom: Math.round(s.bottom), titleBottom: Math.round(t.bottom), leadTop: Math.round(l.top), wrapped }; })()`);
    expect(`markets ${size}`, !head.overlap && head.statsBottom <= head.leadTop, 'the head figures stand on the title line, above the sentence', head);
    expect(`markets ${size}`, head.wrapped === 0, 'no market name wraps in its card', head);
    await noSideScroll(`markets ${size}`);
  }

  for (const size of PHONES) {
    const [w, h] = size.split('x').map(Number);
    await open('#/trade/BTC-USD', w, h, true);
    const where = `trade phone ${size}`;
    await noSideScroll(where);
    const cta = await page.evaluate(`(() => { const c = document.querySelector('.trade-cta'); const r = c.getBoundingClientRect(); const nav = document.querySelector('.nav').getBoundingClientRect(); return { gap: Math.round(nav.top - r.bottom), bottom: Math.round(r.bottom), viewport: innerHeight, alpha: getComputedStyle(c).backgroundColor }; })()`);
    expect(where, cta.gap <= 0 && !/rgba\([^)]*,\s*0?\.\d+\)$/.test(cta.alpha), 'the Long / Short bar is solid and meets the navigation', cta);
    await page.evaluate(`document.querySelector('.trade-cta .tone-long').click()`);
    await delay(700);
    const g = await page.evaluate(GATE);
    expect(where, g && g.outside === 0 && g.footBottom <= g.viewport + 1, 'order sheet: the account figures and the action are whole and on screen', g);
    for (const route of ['#/', '#/markets']) {
      await open(route, w, h, true);
      await noSideScroll(`${route} phone ${size}`);
    }
  }
} finally {
  await browser.close();
}
const summary = { harness: 'fit.mjs (layout acceptance, design pass 3)', app: APP, sizes: SIZES, phones: PHONES, assertions: results.length, failed, results };
if (OUT) writeFileSync(OUT, JSON.stringify(summary, null, 1) + '\n');
console.log(`${results.length} layout assertions, ${failed} failed`);
process.exit(failed ? 1 : 0);
