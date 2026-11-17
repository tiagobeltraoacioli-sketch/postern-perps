#!/usr/bin/env node
// Screenshot and assertion harness of the perps frontend against a loopback mock. UNAUDITED prototype, local only.
//
//   node integration/browser/screens.mjs --app http://127.0.0.1:49111/ --mock http://127.0.0.1:49110 --out <dir> [--port 49150]
//
// A harness wallet (EIP-6963 provider "Harness wallet") is injected into the page; its key is generated for this run
// in this process (viem generatePrivateKey) and signing happens here, never in the page. Every scene asserts the texts
// a user must be able to read on screen without a click, then saves a PNG. Exit 1 on any failed assertion.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';
import { launch, openPage } from './cdp.mjs';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const APP = opt('app', 'http://127.0.0.1:49111/');
const MOCK = opt('mock', 'http://127.0.0.1:49110');
const OUT = opt('out', './.harness');
const PORT = Number(opt('port', '49150'));
const CHROME = opt('chrome', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');
const ONLY = opt('only', '');
mkdirSync(OUT, { recursive: true });

const key = generatePrivateKey();
const wallet = privateKeyToAccount(key);
const ADDRESS = wallet.address;
const L2_HEX = `0x${(700770).toString(16)}`;

const initScript = String.raw`
(() => {
  const address = ${JSON.stringify(ADDRESS)};
  const waiting = new Map();
  let next = 0;
  window.__walletResolve = (id, ok, value) => { const w = waiting.get(id); waiting.delete(id); if (w) ok ? w.resolve(value) : w.reject(Object.assign(new Error(value), { code: 4001 })); };
  const listeners = {};
  const provider = {
    request: async ({ method, params }) => {
      if (method === 'eth_requestAccounts' || method === 'eth_accounts') return [address];
      if (method === 'eth_chainId') return ${JSON.stringify(L2_HEX)};
      if (method === 'eth_signTypedData_v4') {
        const id = ++next;
        const p = new Promise((resolve, reject) => waiting.set(id, { resolve, reject }));
        window.__walletBridge(JSON.stringify({ id, from: params[0], data: params[1] }));
        return p;
      }
      throw Object.assign(new Error('unsupported ' + method), { code: 4200 });
    },
    on: (e, f) => ((listeners[e] ||= []).push(f)),
    removeListener: (e, f) => (listeners[e] = (listeners[e] || []).filter((x) => x !== f)),
  };
  const info = { uuid: '6f1c2b3a-4d5e-4f60-8a7b-9c0d1e2f3a4b', name: 'Harness wallet', icon: '', rdns: 'local.harness' };
  const announce = () => window.dispatchEvent(new CustomEvent('eip6963:announceProvider', { detail: Object.freeze({ info, provider }) }));
  window.addEventListener('eip6963:requestProvider', announce);
  announce();
})();`;

let signatures = 0;
let declineNext = false;
async function onBinding(name, payload, page) {
  if (name !== '__walletBridge') return;
  const { id, from, data } = JSON.parse(payload);
  const typed = JSON.parse(data);
  if (declineNext || from.toLowerCase() !== ADDRESS.toLowerCase()) {
    declineNext = false;
    await page.evaluate(`window.__walletResolve(${id}, false, 'User rejected the request.')`);
    return;
  }
  const fields = typed.types[typed.primaryType];
  const message = Object.fromEntries(fields.map((f) => [f.name, /^u?int/.test(f.type) ? BigInt(typed.message[f.name]) : typed.message[f.name]]));
  const signature = await wallet.signTypedData({ domain: typed.domain, types: { [typed.primaryType]: fields }, primaryType: typed.primaryType, message });
  signatures++;
  await page.evaluate(`window.__walletResolve(${id}, true, ${JSON.stringify(signature)})`);
}

const scenario = async (query) => {
  const r = await fetch(`${MOCK}/mock/scenario?${query}`);
  if (!r.ok) throw new Error(`scenario ${query}: ${r.status}`);
};
const RESET = 'geo=PA&resume=BTC-USD&eip712=pinned&sequencer=running&vault=normal&accounts=reset';
const failures = [];
const results = [];
const expect = (scene, ok, what) => {
  results.push({ scene, ok, what });
  if (!ok) failures.push(`${scene}: ${what}`);
};

const browser = await launch(CHROME, PORT);
const { browserContextId } = await browser.send('Target.createBrowserContext');
const page = await openPage(browser, browserContextId, { initScript, url: 'about:blank', onBinding, log: console.log });
const viewport = (width, height, mobile = false) => page.cdp('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: mobile ? 2 : 1, mobile });
const theme = async (t) => {
  await page.evaluate(`localStorage.setItem('postern.theme.v1', ${JSON.stringify(t)})`);
};
const shot = async (name, full = true) => {
  await delay(450);
  if (full) return page.screenshot(join(OUT, `${name}.png`));
  const { data } = await page.cdp('Page.captureScreenshot', { format: 'png' });
  writeFileSync(join(OUT, `${name}.png`), Buffer.from(data, 'base64'));
};
const visible = () => page.visibleText();
const has = async (scene, pattern) => expect(scene, pattern.test(await visible()), `visible ${pattern}`);
const connect = async () => {
  if (await page.evaluate(`!!window.__h.button('Connect')`)) {
    await page.click('Connect');
    await page.click('Harness wallet');
  }
  await page.waitVisibleText(/Equity/, 15_000);
};
const run = (name) => !ONLY || ONLY.split(',').includes(name);

try {
  await scenario(RESET);
  await viewport(1440, 900);
  await page.goto(APP);
  await theme('dark');
  await page.goto(`${APP}#/trade/BTC-USD`);
  await page.reload();
  await page.waitVisibleText(/Oracle index/, 20_000);
  await page.waitFor('book rows', `document.querySelectorAll('.book-row').length > 10`);

  if (run('01-trade-disconnected')) {
    await has('01', /Connect a wallet to trade/);
    await has('01', /Bloch Inc \(Panama\)/);
    await has('01', /Unaudited prototype/);
    await shot('01-trade-disconnected-dark');
  }
  await connect();
  if (run('02-attestation')) {
    await has('02', /Before your first order/);
    await has('02', /I am not a resident of/);
    await has('02', /ATTEST-0\.1-20261117/);
    await shot('02-attestation-dark');
  }
  // Sign the eligibility statement (397's typed data, 392's text): one wallet signature, checked in the page.
  await page.fill('Country of residence', 'PA');
  await page.evaluate(`document.querySelectorAll('.attestation input[type=checkbox]').forEach((c) => c.click())`);
  await page.click('Sign the statement');
  await page.waitFor('attestation signed', `!document.querySelector('.attestation')`);
  expect('02', signatures === 1, 'the attestation is one wallet signature');
  await page.fill('Size', '0.25');
  await page.fill('Limit price', '67000.5');
  await page.waitVisibleText(/Review long 0\.25 BTC/);
  if (run('03-trade')) {
    await has('03', /Est\. liquidation price/);
    await has('03', /Margin required/);
    await has('03', /Long 0\.5000 BTC|Positions/);
    await shot('03-trade-limit-dark');
  }
  await page.click('Review long');
  await page.waitVisibleText(/Review before signing/);
  if (run('04-review')) {
    await has('04', /Limit price/);
    await has('04', /67,000\.5 USD/);
    await has('04', /Reduce only/);
    await has('04', /cannot move funds out/);
    await has('04', /Estimated liquidation price/);
    await shot('04-review-limit-dark', false);
  }
  await page.click('Sign in wallet and send');
  await page.waitVisibleText(/Included in block|Rejected|Refused/, 15_000);
  expect('05', signatures === 2, 'exactly one wallet signature for one order (after the attestation)');
  await has('05', /Included in block/);
  await shot('05-order-included-dark', false);
  await page.click('Close');

  // Market order: the worst price is shown and signed.
  await page.evaluate(`[...document.querySelectorAll('.mode-tabs [role=tab]')].find((b) => b.textContent === 'Market').click()`);
  await page.fill('Size', '0.1');
  await page.waitVisibleText(/Pays at most/);
  await has('06', /Worst price: mark/);
  await page.click('Review long');
  await page.waitVisibleText(/Worst price accepted/);
  await shot('06-review-market-dark', false);
  await page.click('Cancel');

  // Light theme.
  await theme('light');
  await page.reload();
  await page.waitVisibleText(/Oracle index/, 20_000);
  await connect();
  await shot('07-trade-light');

  // Phone.
  await viewport(390, 844, true);
  await page.reload();
  await page.waitVisibleText(/Oracle index/, 20_000);
  await connect();
  await shot('08-trade-phone-light', false);
  await page.evaluate(`document.querySelector('.order-panel').scrollIntoView({ block: 'start' })`);
  await shot('08b-order-phone-light', false);
  await theme('dark');
  await page.reload();
  await page.waitVisibleText(/Oracle index/, 20_000);
  await connect();
  await shot('09-trade-phone-dark', false);
  await page.evaluate(`document.querySelector('.order-panel').scrollIntoView({ block: 'start' })`);
  await shot('09b-order-phone-dark', false);
  await viewport(1440, 900);

  // Restricted region: banner, exit only, reduce-only forced.
  await scenario('geo=BR');
  await page.reload();
  await page.waitVisibleText(/not available in your region \(BR\)/, 20_000);
  await connect();
  await has('10', /exit only/);
  await shot('10-restricted-dark');
  await scenario('geo=PA');

  // Halted market.
  await scenario('halt=BTC-USD');
  await page.reload();
  await page.waitVisibleText(/BTC-USD is halted/, 20_000);
  await connect();
  await shot('11-halted-dark');
  await scenario('resume=BTC-USD');

  // Typed data differs: banner and refusal.
  await scenario('eip712=changed');
  await page.reload();
  await page.waitVisibleText(/out of date with the exchange/, 20_000);
  await connect();
  await page.fill('Size', '0.1');
  await page.fill('Limit price', '66000');
  await page.click('Review long');
  await page.waitVisibleText(/Signing refused/);
  expect('12', (await page.button('Sign in wallet and send'))?.disabled === true, 'sign button disabled when the typed data differs');
  await shot('12-eip712-mismatch-dark', false);
  await page.click('Cancel');
  await scenario('eip712=pinned');

  for (const [name, hash, pattern] of [
    ['13-portfolio-dark', '#/portfolio', /Margin ratio/],
    ['14-transfers-dark', '#/transfers', /Not live/],
    ['15-agents-dark', '#/agents', /never withdraw/],
    ['16-system-dark', '#/system', /Checks before every signature/],
    ['17-notices-dark', '#/legal', /Where it is not available/],
  ]) {
    await page.goto(`${APP}${hash}`);
    await page.reload();
    await connect().catch(() => {});
    await page.waitVisibleText(pattern, 15_000);
    await shot(name);
  }

  // Console must be clean of errors.
  const errors = page.console.filter((l) => /^(error|pageerror)/.test(l) && !/WebSocket|ERR_CONNECTION|Failed to load resource/.test(l));
  expect('console', errors.length === 0, `no console errors (${errors.slice(0, 3).join(' | ')})`);
} catch (e) {
  failures.push(`harness: ${e.message}`);
  await page.screenshot(join(OUT, 'failure.png')).catch(() => {});
} finally {
  await scenario(RESET).catch(() => {});
  await browser.close();
}
writeFileSync(join(OUT, 'results.json'), JSON.stringify({ results, failures, signatures }, null, 2));
console.log(`${results.length} assertions, ${failures.length} failed, ${signatures} signature(s)`);
for (const f of failures) console.log(`FAIL ${f}`);
process.exit(failures.length ? 1 : 0);
