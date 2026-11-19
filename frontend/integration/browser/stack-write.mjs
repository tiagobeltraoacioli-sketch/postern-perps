#!/usr/bin/env node
// The page WRITES to a real local stack: one deposit, one limit order and one withdrawal, every one of them reviewed
// and sent from the page, through to the real node and the PerpsVault on the stack's anvil. UNAUDITED prototype.
//
//   node integration/browser/stack-write.mjs --app http://127.0.0.1:49813/ --api http://127.0.0.1:49900 \
//        --l2 http://127.0.0.1:49901 --vault 0x… --token 0x… --out DIR [--account-index 9] [--port 49855]
//
// It CHANGES the stack (one account, one deposit, an attestation, an order and its cancel, a withdrawal, one vault
// payout): run it only in a window agreed with whoever runs the stack. What keeps it harmless:
//   - every URL must be a loopback URL, the L2 must answer as anvil on chain 700770 and the API as a dev-mode,
//     unaudited node whose vault is --vault: anything else and nothing is sent;
//   - NO KEY is held or read by this script: the account is one of anvil's own unlocked development accounts
//     (eth_accounts); the page's wallet requests are forwarded to anvil, which signs them (eth_sendTransaction,
//     eth_signTypedData_v4). The harness wallet forwards only transactions from that account to the vault or the
//     token with no value, and typed data of the engine's domain;
//   - the only transaction NOT sent by the page is the mint of mock bUSDC to that account (a test token's public
//     mint), stated in the results.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { decodeFunctionData, encodeFunctionData, parseAbi } from 'viem';
import { launch, openPage } from './cdp.mjs';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const APP = opt('app', 'http://127.0.0.1:49813/');
const API = opt('api', 'http://127.0.0.1:49900');
const L2 = opt('l2', 'http://127.0.0.1:49901');
const VAULT = (opt('vault', '') || '').toLowerCase();
const TOKEN = (opt('token', '') || '').toLowerCase();
const OUT = opt('out', './.stack-write');
const INDEX = Number(opt('account-index', '9'));
const PORT = Number(opt('port', '49855'));
const CHROME = opt('chrome', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');
const fail = (text) => {
  console.error(`stack-write: ${text}`);
  process.exit(2);
};
for (const [name, value] of [['app', APP], ['api', API], ['l2', L2]]) {
  const host = new URL(value).hostname;
  if (host !== '127.0.0.1' && host !== 'localhost') fail(`--${name} must be a loopback URL`);
}
if (!/^0x[0-9a-f]{40}$/.test(VAULT) || !/^0x[0-9a-f]{40}$/.test(TOKEN)) fail('--vault and --token are required');
mkdirSync(OUT, { recursive: true });

let rpcId = 0;
const rpc = async (method, params = []) => {
  const r = await fetch(L2, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: ++rpcId, method, params }) });
  const j = await r.json();
  if (j.error) throw new Error(`${method}: ${j.error.message}`);
  return j.result;
};
const get = async (path) => {
  const r = await fetch(`${API}${path}`);
  return r.ok ? r.json() : null;
};
const TOKEN_ABI = parseAbi(['function mint(address to, uint256 amount)', 'function balanceOf(address) view returns (uint256)', 'function approve(address spender, uint256 amount)', 'function MAX_MINT() view returns (uint256)']);
const balanceOf = async (a) => BigInt(await rpc('eth_call', [{ to: TOKEN, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'balanceOf', args: [a] }) }, 'latest']));
const mined = async (hash) => {
  for (let i = 0; i < 100; i++) {
    const r = await rpc('eth_getTransactionReceipt', [hash]);
    if (r) return r;
    await delay(300);
  }
  throw new Error(`no receipt for ${hash}`);
};

// ---- The stack is what it must be, or nothing is sent.
const client = await rpc('web3_clientVersion');
const chain = await rpc('eth_chainId');
const status = await get('/v1/status');
if (!/^anvil/i.test(client)) fail(`the L2 endpoint is not anvil (${client})`);
if (chain !== '0xab162') fail(`the L2 chain is ${chain}, not 700770`);
if (!status || status.mode !== 'dev' || status.unaudited !== true) fail('the API is not a dev-mode, unaudited node');
if (status.network.vault.toLowerCase() !== VAULT || status.network.collateralToken.toLowerCase() !== TOKEN) fail('the node names another vault or token');
const account = (await rpc('eth_accounts'))[INDEX]?.toLowerCase();
if (!account) fail(`anvil has no development account #${INDEX}`);
const before = await get(`/v1/accounts/${account}`);

const facts = { harness: 'stack-write.mjs (the page writes to a real local stack; UNAUDITED prototype)', startedAt: new Date().toISOString(), app: APP, api: API, l2: L2, l2Client: client, chainId: 700770, vault: VAULT, token: TOKEN, node: { mode: status.mode, apiVersion: status.apiVersion, heightAtStart: status.latestBlock.height }, account, accountIsAnvilDevelopmentAccount: INDEX, keyHeldByThisScript: false, engineAccountBefore: before };
const results = [];
const failures = [];
const expect = (step, ok, what, detail) => {
  results.push({ step, ok: !!ok, what, ...(detail === undefined ? {} : { detail }) });
  if (!ok) failures.push(`${step}: ${what}`);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${step}: ${what}`);
};
const sentByPage = [];
const signedByPage = [];

const initScript = String.raw`
(() => {
  const address = ${JSON.stringify(account)};
  const waiting = new Map();
  let next = 0;
  window.__walletResolve = (id, ok, value) => { const w = waiting.get(id); waiting.delete(id); if (w) ok ? w.resolve(value) : w.reject(Object.assign(new Error(value), { code: 4001 })); };
  const listeners = {};
  const ask = (kind, payload) => { const id = ++next; const p = new Promise((resolve, reject) => waiting.set(id, { resolve, reject })); window.__walletBridge(JSON.stringify({ id, kind, ...payload })); return p; };
  const provider = {
    request: async ({ method, params }) => {
      if (method === 'eth_requestAccounts' || method === 'eth_accounts') return [address];
      if (method === 'eth_chainId') return '0xab162';
      if (method === 'wallet_switchEthereumChain') return null;
      if (method === 'eth_sendTransaction') return ask('tx', { tx: params[0] });
      if (method === 'eth_signTypedData_v4') return ask('typed', { from: params[0], data: params[1] });
      throw Object.assign(new Error('unsupported ' + method), { code: 4200 });
    },
    on: (e, f) => ((listeners[e] ||= []).push(f)),
    removeListener: (e, f) => (listeners[e] = (listeners[e] || []).filter((x) => x !== f)),
  };
  const info = { uuid: '2d7a9c41-5b3e-4f18-9a6c-0e1f2a3b4c5d', name: 'Anvil wallet', icon: '', rdns: 'local.anvil' };
  const announce = () => window.dispatchEvent(new CustomEvent('eip6963:announceProvider', { detail: Object.freeze({ info, provider }) }));
  window.addEventListener('eip6963:requestProvider', announce);
  announce();
})();`;
const VAULT_NAMES = parseAbi(['function deposit(address account, uint256 amount)', 'function withdraw(uint64 seq, (bytes32 withdrawalId, address account, address recipient, uint256 amount, uint256 fee) record, bytes32[] siblings)', 'function release(bytes32 key)']);
async function onBinding(name, payload, p) {
  if (name !== '__walletBridge') return;
  const m = JSON.parse(payload);
  const refuse = (why) => p.evaluate(`window.__walletResolve(${m.id}, false, ${JSON.stringify(why)})`);
  try {
    if (m.kind === 'tx') {
      const { tx } = m;
      const to = (tx.to ?? '').toLowerCase();
      if ((tx.from ?? '').toLowerCase() !== account || (to !== VAULT && to !== TOKEN) || (tx.value ?? '0x0') !== '0x0') return refuse('refused by the harness wallet');
      let fn = tx.data.slice(0, 10);
      try {
        fn = decodeFunctionData({ abi: to === TOKEN ? TOKEN_ABI : VAULT_NAMES, data: tx.data }).functionName;
      } catch {
        /* the selector is recorded as it is */
      }
      const hash = await rpc('eth_sendTransaction', [tx]);
      sentByPage.push({ to: to === TOKEN ? 'token' : 'vault', call: fn, hash });
      return p.evaluate(`window.__walletResolve(${m.id}, true, ${JSON.stringify(hash)})`);
    }
    const typed = JSON.parse(m.data);
    if (m.from.toLowerCase() !== account || typed.domain?.name !== 'PosternPerps' || Number(typed.domain?.chainId) !== 700770 || (typed.domain?.verifyingContract ?? '').toLowerCase() !== VAULT) return refuse('refused by the harness wallet');
    const signature = await rpc('eth_signTypedData_v4', [account, typed]);
    signedByPage.push({ primaryType: typed.primaryType, message: typed.message });
    return p.evaluate(`window.__walletResolve(${m.id}, true, ${JSON.stringify(signature)})`);
  } catch (e) {
    return refuse(String(e.message ?? e));
  }
}

const browser = await launch(CHROME, PORT);
let page;
const shot = async (name) => {
  await delay(500);
  const { data } = await page.cdp('Page.captureScreenshot', { format: 'png' });
  writeFileSync(join(OUT, `${name}.png`), Buffer.from(data, 'base64'));
};
const sheetSend = async (title) => {
  await page.waitVisibleText(/Review before sending · Bloch L2 transaction/, 30_000);
  await page.waitVisibleText(title, 15_000);
  await page.click('Send in wallet');
  await page.waitVisibleText(/In L2 block \d+/, 90_000);
  await page.click('Close');
};
const until = async (what, read, ok, timeoutMs) => {
  const end = Date.now() + timeoutMs;
  let last;
  while (Date.now() < end) {
    last = await read();
    if (ok(last)) return last;
    await delay(1500);
  }
  throw new Error(`timeout waiting for ${what}: ${JSON.stringify(last)?.slice(0, 300)}`);
};

try {
  const { browserContextId } = await browser.send('Target.createBrowserContext');
  page = await openPage(browser, browserContextId, { initScript, url: 'about:blank', onBinding, log: () => {} });
  await page.cdp('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
  await page.goto(APP);
  await page.evaluate(`localStorage.setItem('postern.theme.v1', 'dark')`);

  // ---- 0. Test money: the mock token's public mint (the one transaction the page does not send).
  const want = 2_000_000_000n; // 2,000.000000 bUSDC
  const had = await balanceOf(account);
  if (had < 1_100_000_000n) {
    const mint = await rpc('eth_sendTransaction', [{ from: account, to: TOKEN, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'mint', args: [account, want] }) }]);
    const receipt = await mined(mint);
    facts.mintByHarness = { hash: mint, block: Number(receipt.blockNumber), status: receipt.status, amount: want.toString() };
    expect('0 mint', receipt.status === '0x1', 'mock bUSDC minted to the harness account (by the harness, not the page)');
  }
  const tokenBefore = await balanceOf(account);

  // ---- 1. Deposit, from the page.
  await page.goto(`${APP}#/transfers/deposit`);
  await page.reload();
  await page.click('Connect');
  await page.click('Anvil wallet');
  await page.waitVisibleText(/Vault open/, 40_000);
  await page.fill('Amount', '1000');
  await shot('w01-deposit-ready');
  if (await page.evaluate(`!!window.__h.button('Review allowance of 1,000.00 bUSDC')`)) {
    await page.click('Review allowance of 1,000.00 bUSDC');
    await sheetSend(/Allow the vault to take 1,000\.00 bUSDC from this wallet/);
  }
  await page.click('Review deposit of 1,000.00 bUSDC', 40_000);
  await page.waitVisibleText(new RegExp(`Credited to trading account\\s*${account}`, 'i'));
  await shot('w02-deposit-sheet');
  await sheetSend(/Deposit 1,000\.00 bUSDC into the exchange vault/);
  await page.waitVisibleText(/Vault deposit #\d+/, 60_000);
  expect('1 deposit', (await balanceOf(account)) === tokenBefore - 1_000_000_000n, 'the vault took exactly 1,000.00 bUSDC from the wallet', { before: tokenBefore.toString(), after: (await balanceOf(account)).toString() });
  expect('1 deposit', sentByPage.some((t) => t.call === 'deposit'), 'the deposit transaction was sent by the page', sentByPage);
  // The REAL node credits it once the L2 block is final for it.
  const credited = await until('the node to credit the deposit', () => get(`/v1/accounts/${account}`), (a) => !!a && Number(a.collateral) > 0, 240_000).catch((e) => ({ error: String(e.message) }));
  facts.engineAccountAfterDeposit = credited;
  expect('1 deposit', !credited.error, 'the node credited the deposit to the trading account', credited.error);
  await page.waitVisibleText(/1,000\.00 bUSDC\s*Credited/, 120_000).catch(() => {});
  await shot('w03-deposit-credited');

  // ---- 2. One limit order, from the page (after the eligibility statement, itself signed in the page).
  await page.goto(`${APP}#/trade/BTC-USD`);
  await page.reload();
  await page.waitVisibleText(/Before your first order|Review|Long BTC/, 40_000);
  if (/Before your first order/.test(await page.visibleText())) {
    await page.fill('Country of residence', 'PA');
    await page.evaluate(`document.querySelectorAll('.attestation input[type=checkbox]').forEach((c) => c.click())`);
    await page.click('Sign the statement');
    await page.waitFor('attestation signed', `!document.querySelector('.attestation')`, 30_000);
  }
  const market = (await get('/v1/markets/BTC-USD')) ?? {};
  const ticker = (await get('/v1/markets/BTC-USD/ticker')) ?? {};
  const mark = Number(ticker.markPrice);
  if (!(mark > 0)) throw new Error('the node gave no mark price for BTC-USD');
  // Well under the mark and inside the price band: it rests, it does not trade.
  const limit = (Math.floor((mark * 0.97) / 0.5) * 0.5).toFixed(1);
  facts.order = { mark, limit, size: '0.01', marketStatus: market.status };
  await page.fill('Size', '0.01');
  await page.fill('Limit price', limit);
  await page.waitVisibleText(/Review long 0\.01 BTC/, 20_000);
  await shot('w04-order-form');
  await page.click('Review long');
  await page.waitVisibleText(/Review before signing/, 20_000);
  await shot('w05-order-review');
  await page.click('Sign in wallet and send');
  await page.waitVisibleText(/Included in block|Rejected|Refused/, 40_000);
  const orderText = await page.visibleText();
  expect('2 order', /Included in block/.test(orderText), 'the order signed in the page was included by the real node', /Included in block \d+/.exec(orderText)?.[0] ?? orderText.slice(0, 300));
  await shot('w06-order-included');
  await page.click('Close');
  const orders = await get(`/v1/accounts/${account}/orders`);
  facts.openOrdersAfter = orders;
  expect('2 order', (orders?.orders ?? []).some((o) => o.symbol === 'BTC-USD'), 'the node lists the resting order of this account', (orders?.orders ?? []).length);
  expect('2 order', signedByPage.some((s) => s.primaryType === 'Order' || s.primaryType === 'PlaceOrder'), 'the order was one typed-data signature asked by the page', signedByPage.map((s) => s.primaryType));

  // ---- 3. One withdrawal, from the page: the signed action, then the payout collected from the vault.
  await page.goto(`${APP}#/transfers/withdraw`);
  await page.waitVisibleText(/Withdraw to the Bloch L2/, 30_000);
  await page.fill('Amount', '100');
  await shot('w07-withdraw-form');
  await page.click('Review withdrawal', 30_000);
  await page.waitVisibleText(/Review before signing/, 20_000);
  await shot('w08-withdraw-review');
  await page.click('Sign in wallet and send');
  await page.waitVisibleText(/Included in block|Rejected|Refused/, 40_000);
  const wText = await page.visibleText();
  expect('3 withdraw', /Included in block/.test(wText), 'the withdrawal signed in the page was included by the real node', /Included in block \d+/.exec(wText)?.[0] ?? wText.slice(0, 300));
  await page.click('Close');
  const walletBefore = await balanceOf(account);
  // The exchange certifies it in a checkpoint the vault accepts; then the page offers the collection.
  const offered = await page.waitFor('the vault payout to be offered', `(() => { const b = window.__h.button('Collect from the vault'); return !!b && !b.disabled; })()`, 420_000).then(() => true, (e) => String(e.message));
  expect('3 withdraw', offered === true, 'the page offers "Collect from the vault" once the vault accepted the checkpoint', offered);
  if (offered === true) {
    await shot('w09-withdraw-certified');
    await page.click('Collect from the vault');
    await sheetSend(/withdraw|Collect|pay/i);
    const paid = await until('the vault payout', () => balanceOf(account).then(String), (b) => BigInt(b) > walletBefore, 60_000).catch((e) => ({ error: String(e.message) }));
    facts.payout = { walletBefore: walletBefore.toString(), walletAfter: paid.error ? null : paid };
    expect('3 withdraw', !paid.error, 'the vault paid the withdrawal to the wallet on the L2', facts.payout);
    expect('3 withdraw', sentByPage.some((t) => t.call === 'withdraw'), 'the vault transaction was sent by the page', sentByPage);
    await shot('w10-withdraw-paid');
  }

  // ---- 4. Leave the book as it was found: cancel the resting order, from the page.
  await page.goto(`${APP}#/trade/BTC-USD`);
  await page.waitVisibleText(/Open orders/, 30_000);
  await page.evaluate(`[...document.querySelectorAll('.account-tables [role=tab]')].find((b) => b.textContent.startsWith('Open orders')).click()`);
  await page.click('Cancel all', 20_000).catch(() => {});
  if (/Review before signing/.test(await page.visibleText())) {
    await page.click('Sign in wallet and send');
    await page.waitVisibleText(/Included in block|Rejected|Refused/, 40_000);
    await page.click('Close').catch(() => {});
  }
  const left = await get(`/v1/accounts/${account}/orders`);
  expect('4 cancel', (left?.orders ?? []).length === 0, 'no order of the harness account is left in the book', (left?.orders ?? []).length);
  const errors = page.console.filter((l) => /^(error|pageerror)/.test(l) && !/Failed to load resource|WebSocket/.test(l));
  expect('console', errors.length === 0, 'no error in the page console', errors.slice(0, 5));
} catch (e) {
  expect('run', false, `stopped: ${String(e.message ?? e).slice(0, 400)}`);
  if (page) await shot('w99-stopped').catch(() => {});
} finally {
  facts.finishedAt = new Date().toISOString();
  facts.engineAccountAtEnd = await get(`/v1/accounts/${account}`).catch(() => null);
  facts.transactionsThePageSent = sentByPage;
  facts.typedDataThePageSigned = signedByPage;
  writeFileSync(join(OUT, 'results.json'), JSON.stringify({ facts, results, failures }, null, 1) + '\n');
  await browser.close();
}
console.log(`${results.length} assertions, ${failures.length} failed; account ${account}`);
process.exit(failures.length ? 1 : 0);
