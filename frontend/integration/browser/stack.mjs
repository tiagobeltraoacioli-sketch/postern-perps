#!/usr/bin/env node
// READ-ONLY run of the frontend against a real local stack (431's dev node, 433's vault on anvil, through 435's
// e2e/up.sh or any equal setup). UNAUDITED prototype, loopback only.
//
//   node integration/browser/stack.mjs --app http://127.0.0.1:49813/ --api http://127.0.0.1:49900 --out <dir>
//        [--account 0x…] [--port 49851]
//
// It changes nothing in the stack: the injected wallet is WATCH-ONLY (it reports --account, or the account with the
// most fills that the node's own blocks name, and declines every signature and transaction with 4001), and the run
// FAILS if the page asks it for one. What it proves: every panel is filled from the engine and the vault (not from a
// mock), the pinned typed data and market table equal what the real node publishes, and the vault is read from the L2.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { launch, openPage } from './cdp.mjs';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const APP = opt('app', 'http://127.0.0.1:49920/');
const API = opt('api', 'http://127.0.0.1:49900');
const OUT = opt('out', './.harness-stack');
const PORT = Number(opt('port', '49851'));
const CHROME = opt('chrome', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');
mkdirSync(OUT, { recursive: true });
const get = async (path) => (await fetch(`${API}${path}`)).json();

const status = await get('/v1/status');
let account = opt('account', '');
if (!account) {
  // The account with the most fills among those the node's blocks name (deposits and actions).
  const seen = new Set();
  const head = status.latestBlock.height;
  for (let from = 1; from <= head && seen.size < 40; from += 500) {
    const { blocks } = await get(`/v1/blocks?from=${from}&limit=500`);
    for (const b of blocks) for (const i of b.inputs) if (i.kind === 'deposit') seen.add(i.payload.account); else if (i.kind === 'action') seen.add(i.account);
  }
  let best = -1;
  for (const a of seen) {
    const acct = await get(`/v1/accounts/${a}`);
    const { fills } = await get(`/v1/accounts/${a}/fills?limit=500`);
    const score = fills.length + acct.positions.length * 100 + acct.openOrders * 10;
    if (score > best) [best, account] = [score, a];
  }
}
if (!/^0x[0-9a-fA-F]{40}$/.test(account)) throw new Error('no account to watch');
const facts = { account, node: { mode: status.mode, apiVersion: status.apiVersion, height: status.latestBlock.height, vault: status.network.vault } };
facts.engine = await get(`/v1/accounts/${account}`);
facts.fills = (await get(`/v1/accounts/${account}/fills?limit=500`)).fills.length;
facts.funding = (await get(`/v1/accounts/${account}/funding?limit=500`)).payments.length;
facts.deposits = (await get(`/v1/accounts/${account}/deposits`)).deposits.length;

const initScript = String.raw`
(() => {
  const address = ${JSON.stringify(account)};
  window.__walletAsked = [];
  const listeners = {};
  const provider = {
    request: async ({ method }) => {
      if (method === 'eth_requestAccounts' || method === 'eth_accounts') return [address];
      if (method === 'eth_chainId') return '0xab162';
      window.__walletAsked.push(method);
      throw Object.assign(new Error('watch-only wallet'), { code: 4001 });
    },
    on: (e, f) => ((listeners[e] ||= []).push(f)),
    removeListener: (e, f) => (listeners[e] = (listeners[e] || []).filter((x) => x !== f)),
  };
  const info = { uuid: '7a2d3c4b-5e6f-4a71-9b8c-0d1e2f3a4b5c', name: 'Watch-only wallet', icon: '', rdns: 'local.watch' };
  const announce = () => window.dispatchEvent(new CustomEvent('eip6963:announceProvider', { detail: Object.freeze({ info, provider }) }));
  window.addEventListener('eip6963:requestProvider', announce);
  announce();
})();`;

const failures = [];
const results = [];
const expect = (scene, ok, what) => {
  results.push({ scene, ok, what });
  if (!ok) failures.push(`${scene}: ${what}`);
};
const browser = await launch(CHROME, PORT);
const { browserContextId } = await browser.send('Target.createBrowserContext');
const page = await openPage(browser, browserContextId, { initScript, url: 'about:blank', onBinding: async () => {}, log: () => {} });
const shot = async (name, full = true) => {
  await delay(600);
  if (full) return page.screenshot(join(OUT, `${name}.png`));
  const { data } = await page.cdp('Page.captureScreenshot', { format: 'png' });
  writeFileSync(join(OUT, `${name}.png`), Buffer.from(data, 'base64'));
};
const has = async (scene, pattern) => expect(scene, pattern.test(await page.visibleText()), `visible ${pattern}`);
const lacks = async (scene, pattern) => expect(scene, !pattern.test(await page.visibleText()), `not visible ${pattern}`);
const connect = async () => {
  if (await page.evaluate(`!!window.__h.button('Connect')`)) {
    await page.click('Connect');
    await page.waitVisibleText(/Connecting only shows your address to this page/);
    await page.click('Watch-only wallet');
  }
};
const group = (n) => n.replace(/\B(?=(\d{3})+(?!\d))/g, ',');

try {
  await page.cdp('Emulation.setDeviceMetricsOverride', { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
  await page.goto(APP);
  await page.evaluate(`localStorage.setItem('postern.theme.v1', 'dark')`);
  const symbol = facts.engine.positions[0]?.symbol ?? 'BTC-USD';
  await page.goto(`${APP}#/trade/${symbol}`);
  await page.reload();
  await page.waitVisibleText(/Oracle index/, 30_000);
  await connect();
  await page.waitVisibleText(/Equity/, 20_000);
  // The pins of this build equal what the REAL node publishes: no banner, and the order form is live.
  await lacks('trade', /out of date with the exchange/);
  await lacks('trade', /Signing is refused on/);
  await lacks('trade', /Not live: the exchange's vault is not deployed/);
  await page.waitFor('book or empty book', `document.querySelectorAll('.book-row').length > 0 || /Reading the book/.test(document.body.innerText) === false`, 20_000).catch(() => {});
  facts.bookRows = await page.evaluate(`document.querySelectorAll('.book-row').length`);
  facts.candlesDrawn = await page.evaluate(`!/No candles yet|Reading candles/.test(document.querySelector('.chart-panel')?.innerText ?? '')`);
  const equity = group(facts.engine.equity.split('.')[0]);
  await has('trade', new RegExp(equity.replace(/,/g, ',')));
  for (const p of facts.engine.positions) await has('trade', new RegExp(p.symbol));
  if (facts.engine.positions.length) await has('trade', /Close at market/);
  await shot('s01-trade-real-node', false);
  for (const tab of ['Open orders', 'Fills', 'Funding']) {
    await page.evaluate(`[...document.querySelectorAll('.account-tables [role=tab]')].find((b) => b.textContent.startsWith(${JSON.stringify(tab)})).click()`);
    await delay(300);
    facts[`rows ${tab}`] = await page.evaluate(`document.querySelectorAll('.account-tables tbody tr:not(:has(.empty))').length`);
  }
  expect('trade', facts['rows Open orders'] === facts.engine.openOrders, `open orders shown ${facts['rows Open orders']} = engine ${facts.engine.openOrders}`);
  expect('trade', facts['rows Fills'] === facts.fills, `fills shown ${facts['rows Fills']} = engine ${facts.fills}`);
  expect('trade', facts['rows Funding'] === Math.min(facts.funding, 300), `funding payments shown ${facts['rows Funding']} = engine ${facts.funding}`);
  await shot('s02-trade-fills-real-node', false);

  await page.goto(`${APP}#/portfolio`);
  await page.waitVisibleText(/Margin ratio/, 15_000);
  await has('portfolio', new RegExp(`${equity}\\.\\d\\d USDC`));
  await shot('s03-portfolio-real-node');

  await page.goto(`${APP}#/transfers/deposit`);
  await page.waitVisibleText(/Held by the vault/, 30_000);
  await has('funds', /Vault open|Vault paused|Exchange overdue|Escape mode/);
  await has('funds', /Read at L2 block/);
  await lacks('funds', /The vault is not deployed/);
  await page.waitVisibleText(/Vault deposit #\d+/, 20_000).catch(() => {});
  facts.vaultDepositsShown = await page.evaluate(`[...document.querySelectorAll('.records li')].filter((li) => /Vault deposit #/.test(li.innerText)).length`);
  expect('funds', facts.vaultDepositsShown === facts.deposits, `deposits from the vault's own log ${facts.vaultDepositsShown} = engine deposits ${facts.deposits}`);
  if (facts.deposits > 0) await has('funds', /Credited/);
  await shot('s04-funds-deposit-real-vault');
  await page.goto(`${APP}#/transfers/withdraw`);
  await page.waitVisibleText(/Your withdrawals/, 15_000);
  await shot('s05-funds-withdraw-real-vault');
  await page.goto(`${APP}#/transfers/exit`);
  await page.waitVisibleText(/Escape is not due|The exchange is overdue|Escape mode since/, 30_000);
  await page.waitVisibleText(/Saved on this device for the vault's latest checkpoint|Nothing to keep yet|gave no usable proof/, 20_000);
  await has('exit', /Your exit proof/);
  facts.exitProof = (await page.visibleText()).match(/Saved on this device for the vault's latest checkpoint \(exchange height (\d+)\)|Nothing to keep yet|gave no usable proof/)?.[0] ?? null;
  await shot('s06-funds-exit-real-vault');

  await page.goto(`${APP}#/history/fills`);
  await page.waitVisibleText(/Fees paid/, 15_000);
  await shot('s07-history-fills-real-node');
  await page.goto(`${APP}#/funding/${symbol}`);
  await page.waitVisibleText(/Settled rates, one per/, 15_000);
  await page.waitFor('funding bars or none', `document.querySelectorAll('.funding-chart rect.funding-up, .funding-chart rect.funding-down').length > 0 || /No funding has settled|could not be read/.test(document.body.innerText)`, 20_000).catch(() => {});
  facts.fundingBars = await page.evaluate(`document.querySelectorAll('.funding-chart rect.funding-up, .funding-chart rect.funding-down').length`);
  await shot('s08-funding-real-node');
  await page.goto(`${APP}#/markets`);
  await page.waitVisibleText(/Perpetual markets/, 15_000);
  expect('markets', (await page.evaluate(`document.querySelectorAll('.markets-table tbody tr').length`)) === (await get('/v1/markets')).markets.length, 'every market of the engine is listed');
  await lacks('markets', /signing refused/);
  await shot('s09-markets-real-node');
  await page.goto(`${APP}#/system`);
  await page.waitVisibleText(/Checks before every signature/, 15_000);
  await shot('s10-system-real-node');

  facts.walletAsked = JSON.parse(await page.evaluate(`JSON.stringify(window.__walletAsked)`));
  expect('read-only', facts.walletAsked.length === 0, `the wallet was asked for nothing (${facts.walletAsked.join()})`);
  const errors = page.console.filter((l) => /^(error|pageerror)/.test(l) && !/Failed to load resource/.test(l));
  expect('console', errors.length === 0, `no console errors (${errors.slice(0, 3).join(' | ')})`);
} catch (e) {
  failures.push(`harness: ${e.message}`);
  await page.screenshot(join(OUT, 'failure.png')).catch(() => {});
} finally {
  await browser.close();
}
writeFileSync(join(OUT, 'results.json'), JSON.stringify({ facts: { ...facts, engine: { ...facts.engine, positions: facts.engine?.positions?.length } }, results, failures }, null, 2));
console.log(`${results.length} assertions, ${failures.length} failed; watched ${account}`);
for (const f of failures) console.log(`FAIL ${f}`);
process.exit(failures.length ? 1 : 0);
