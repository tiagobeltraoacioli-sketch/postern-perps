#!/usr/bin/env node
// Screenshot and assertion harness of the perps frontend against a loopback mock. UNAUDITED prototype, local only.
//
//   node integration/browser/screens.mjs --app http://127.0.0.1:49111/ --mock http://127.0.0.1:49110 --out <dir> [--port 49150]
// The mock must run on the real clock (node/mock: --start-time now, not --static): the page refuses to sign when the
// exchange clock is outside the SPEC 2.5 nonce window of this device clock.
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
  let chain = ${JSON.stringify(L2_HEX)};
  let known = [chain];
  window.__walletAsked = [];
  const setChain = (c) => { chain = c; (listeners.chainChanged || []).forEach((f) => f(c)); };
  // Test control: put the wallet on another network that does not know the Bloch L2.
  window.__walletElsewhere = (c) => { known = [c]; setChain(c); };
  window.__walletResolve = (id, ok, value) => { const w = waiting.get(id); waiting.delete(id); if (w) ok ? w.resolve(value) : w.reject(Object.assign(new Error(value), { code: 4001 })); };
  const listeners = {};
  const provider = {
    request: async ({ method, params }) => {
      if (method === 'eth_requestAccounts' || method === 'eth_accounts') return [address];
      if (method === 'eth_chainId') return chain;
      // Round 2: the harness wallet has a network. It knows only the chains in 'known'; a switch to another answers
      // 4902 until the page adds it (EIP-3085). What the page asked is recorded for the assertions.
      if (method === 'wallet_switchEthereumChain' || method === 'wallet_addEthereumChain') {
        window.__walletAsked.push({ method, params });
        const want = params[0].chainId;
        if (method === 'wallet_switchEthereumChain' && !known.includes(want)) throw Object.assign(new Error('Unrecognized chain'), { code: 4902 });
        if (!known.includes(want)) known.push(want);
        setChain(want);
        return null;
      }
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
const RESET = 'geo=ZZ&resume=BTC-USD&eip712=pinned&sequencer=running&vault=normal';
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
    await page.waitVisibleText(/Connecting only shows your address to this page/);
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

  // Restricted region: the restricted screen first (what is off, what stays open, the exits), then exit-only.
  await scenario('geo=BR');
  await page.reload();
  await page.waitVisibleText(/not available in your region \(BR\)/, 20_000);
  await has('10', /PosternDEX Perpetuals is not offered where you are/);
  await has('10', /What always stays open/);
  await has('10', /Withdrawing your collateral/);
  await has('10', /The exit on the Bloch L2, which no region rule can block/);
  expect('10', !(await page.evaluate(`!!document.querySelector('.order-panel')`)), 'no order form behind the restricted screen');
  await shot('10a-restricted-screen-dark', false);
  await page.click('Continue to manage my account');
  await page.waitVisibleText(/Oracle index/, 20_000);
  await connect();
  await has('10', /exit only/);
  await has('10', /not available in your region \(BR\)/);
  await shot('10-restricted-dark');
  // No new deposit is offered to a restricted region; withdraw and exit stay.
  await page.goto(`${APP}#/transfers/withdraw`);
  await page.waitVisibleText(/Review withdrawal/, 15_000);
  await has('10', /exit without the exchange/);
  await scenario('geo=ZZ');
  await page.goto(`${APP}#/trade/BTC-USD`);

  // Round 2: a wallet on another network. Header says so, an order is refused in words, the switch adds the L2.
  await page.reload();
  await page.waitVisibleText(/Oracle index/, 20_000);
  await connect();
  await page.evaluate(`window.__walletElsewhere('0x1')`);
  await page.waitVisibleText(/Wrong network/, 10_000);
  await page.fill('Size', '0.1');
  await page.fill('Limit price', '66000');
  await page.click('Review long');
  await page.waitVisibleText(/Signing refused/);
  await has('10n', /Your wallet is on another network\. Switch it to the Bloch L2 \(chain 700770\)/);
  expect('10n', (await page.button('Sign in wallet and send'))?.disabled === true, 'sign button disabled on another network');
  await shot('10n-wrong-network-review-dark', false);
  await page.click('Cancel');
  await page.click('Wrong network');
  await page.waitVisibleText(/On chain 1, not the Bloch L2/);
  await shot('10o-wrong-network-menu-dark', false);
  await page.click('Switch to Bloch L2');
  await page.waitFor('wallet back on the L2', `!document.body.innerText.includes('Wrong network')`);
  const asked = JSON.parse(await page.evaluate(`JSON.stringify(window.__walletAsked)`));
  expect('10n', asked.map((a) => a.method).join() === 'wallet_switchEthereumChain,wallet_addEthereumChain', `switch then add (${asked.map((a) => a.method).join()})`);
  const added = asked[1]?.params?.[0] ?? {};
  expect('10n', added.chainId === L2_HEX && added.chainName === 'Bloch L2' && added.nativeCurrency?.symbol === 'BLCH' && added.nativeCurrency?.decimals === 18 && Array.isArray(added.rpcUrls) && added.rpcUrls.length === 1, `the network entry offered to the wallet (${JSON.stringify(added)})`);
  await page.evaluate(`document.body.click()`);

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

  const PAGES = [
    ['13-portfolio', '#/portfolio', /Margin ratio/],
    ['14-transfers', '#/transfers', /Not live/],
    ['14b-funds-withdraw', '#/transfers/withdraw', /Your withdrawals/],
    ['14c-funds-exit', '#/transfers/exit', /Your funds can always leave, even if the exchange is silent/],
    ['15-agents', '#/agents', /never withdraw/],
    ['16-system', '#/system', /Checks before every signature/],
    ['17-notices', '#/legal', /Where it is not available/],
    ['20-markets', '#/markets', /Perpetual markets/],
    ['21-history-orders', '#/history', /Account history/],
    ['21b-history-fills', '#/history/fills', /Fees paid/],
    ['21c-history-funding', '#/history/funding', /Net funding/],
    ['21d-history-transfers', '#/history/transfers', /Recipient/],
    ['22-funding', '#/funding/BTC-USD', /Longs pay shorts/],
    ['23-notfound', '#/no-such-page', /No gate at this address/],
  ];
  for (const [name, hash, pattern] of PAGES) {
    await page.goto(`${APP}${hash}`);
    await page.reload();
    await connect().catch(() => {});
    await page.waitVisibleText(pattern, 15_000);
    await shot(`${name}-dark`);
  }
  // The exit page says what each stage is, in plain words, with or without a vault.
  await page.goto(`${APP}#/transfers/exit`);
  await page.waitVisibleText(/Ask on the L2/, 15_000);
  for (const text of [/The exchange must process it within/, /anyone can switch the vault to escape mode/, /Users are paid before the insurance fund/]) await has('14c', text);
  // An unknown address is never shown as another page.
  for (const bad of ['#/trade/btc-usd', '#/transfers/steal', '#/history/x', '#/portfolio/extra', '#/markets/1']) {
    await page.goto(`${APP}${bad}`);
    await page.waitVisibleText(/No gate at this address/, 10_000);
  }
  expect('23', true, 'five malformed addresses show the 404 page');
  // The markets table sorts and a row opens its market.
  await page.goto(`${APP}#/markets`);
  await page.waitVisibleText(/Perpetual markets/, 15_000);
  expect('20', (await page.evaluate(`document.querySelectorAll('.markets-table tbody tr').length`)) >= 3, 'at least three markets listed');
  await page.click('24 h volume');
  expect('20', (await page.evaluate(`document.querySelector('th[aria-sort=descending]')?.textContent ?? ''`)).includes('24 h volume'), 'sorted by volume, descending, announced with aria-sort');
  await page.evaluate(`[...document.querySelectorAll('.market-cell')].find((b) => b.textContent.includes('ETH-USD')).click()`);
  await page.waitVisibleText(/ETH-USD/, 10_000);
  expect('20', (await page.evaluate(`location.hash`)) === '#/trade/ETH-USD', 'a market row opens that market');

  // Light theme and phone for the new pages; no page scrolls sideways at 360 px.
  await theme('light');
  for (const [name, hash, pattern] of PAGES.filter(([n]) => /^(13|14c|20|22)/.test(n))) {
    await page.goto(`${APP}${hash}`);
    await page.reload();
    await connect().catch(() => {});
    await page.waitVisibleText(pattern, 15_000);
    await shot(`${name}-light`);
  }
  await theme('dark');
  await viewport(360, 780, true);
  for (const [name, hash, pattern] of [['03-trade', '#/trade/BTC-USD', /Oracle index/], ...PAGES]) {
    await page.goto(`${APP}${hash}`);
    await page.reload();
    await connect().catch(() => {});
    await page.waitVisibleText(pattern, 15_000);
    const overflow = await page.evaluate(`document.documentElement.scrollWidth - window.innerWidth`);
    expect(name, overflow <= 0, `no sideways scroll at 360 px (${overflow} px over)`);
    if (/^(03|13|14b|14c|20|21-|22|23)/.test(name)) await shot(`${name}-phone360-dark`, false);
  }
  // The phone "More" menu reaches every page that is not in the bottom bar.
  await page.click('More');
  for (const label of [/History/, /Funding/, /Agent keys/, /System/, /Notices and risk/]) await has('nav', label);
  await shot('24-more-menu-phone360-dark', false);
  await viewport(1440, 900);

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
