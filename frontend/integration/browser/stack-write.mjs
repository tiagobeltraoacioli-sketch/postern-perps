#!/usr/bin/env node
// The page WRITES to a real local stack: deposits, a limit order that FILLS against a second account's resting order,
// and one withdrawal collected from the vault, every one of them reviewed and sent from the page, through to the
// real node and the PerpsVault on the stack's anvil. UNAUDITED prototype.
//
//   node integration/browser/stack-write.mjs --app http://127.0.0.1:49920/ --api http://127.0.0.1:49900 \
//        --l2 http://127.0.0.1:49901 --vault 0x… --token 0x… --out DIR \
//        [--account-index 9] [--maker-index 8] [--port 49855] [--withdraw-timeout-s 600] [--rehearse]
//
// It CHANGES the stack (two accounts, two deposits, two attestations, two orders and one fill, a withdrawal, one
// vault payout; both accounts are left with a 0.01 BTC position, the taker long and the maker short): run it only in
// a window agreed with whoever runs the stack. What keeps it harmless:
//   - every URL must be a loopback URL, the L2 must answer as anvil on chain 700770 and the API as a dev-mode,
//     unaudited node whose vault is --vault: anything else and nothing is sent;
//   - NO KEY is held or read by this script: the accounts are two of anvil's own unlocked development accounts
//     (eth_accounts); the page's wallet requests are forwarded to anvil, which signs them (eth_sendTransaction,
//     eth_signTypedData_v4). The harness wallet forwards only transactions from its account to the vault or the
//     token with no value, and typed data of the engine's domain;
//   - the only transactions NOT sent by the page are the mints of mock bUSDC (a test token's public mint), stated
//     in the results.
// The vault's functions are named from the PINNED ABI the page itself uses (config/vault-abi.json): this script
// keeps no copy of any vault signature (a hand-written copy was wrong in its first version; src/test/pass3.test.ts).
//
// --rehearse: a LOCAL dry run against the reference mock and a local anvil vault (integration/vault/
// stack-write.test.ts starts both). The mock is not a node: it does not watch the L2 and its withdrawal certificate
// is not for this vault, so the two waits on the node's credit and the vault payout are SKIPPED and reported as
// "not rehearsed"; every other step (wallets, reconnects, both deposits on the real vault bytecode, attestations,
// both orders, the fill, the withdrawal action, the cancel) runs as it will against the real stack.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { decodeFunctionData, decodeFunctionResult, encodeFunctionData, parseAbi } from 'viem';
import { launch, openPage } from './cdp.mjs';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const APP = opt('app', 'http://127.0.0.1:49920/');
const API = opt('api', 'http://127.0.0.1:49900');
const L2 = opt('l2', 'http://127.0.0.1:49901');
const VAULT = (opt('vault', '') || '').toLowerCase();
const TOKEN = (opt('token', '') || '').toLowerCase();
const OUT = opt('out', './.stack-write');
const TAKER_INDEX = Number(opt('account-index', '9'));
const MAKER_INDEX = Number(opt('maker-index', '8'));
const PORT = Number(opt('port', '49855'));
const WITHDRAW_TIMEOUT_MS = Number(opt('withdraw-timeout-s', '600')) * 1000;
const REHEARSE = args.includes('--rehearse');
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

// The vault ABI: the pinned file of the page (never a copy). The token: the mock token's mint and the ERC-20 calls.
const VAULT_ABI = JSON.parse(readFileSync(new URL('../../config/vault-abi.json', import.meta.url), 'utf8')).abi;
const TOKEN_ABI = parseAbi(['function mint(address to, uint256 amount)', 'function balanceOf(address) view returns (uint256)', 'function approve(address spender, uint256 amount) returns (bool)']);

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
const balanceOf = async (a) => BigInt(await rpc('eth_call', [{ to: TOKEN, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'balanceOf', args: [a] }) }, 'latest']));
/** A view of the vault by its pinned ABI. */
const vaultRead = async (functionName, fnArgs = []) => decodeFunctionResult({ abi: VAULT_ABI, functionName, data: await rpc('eth_call', [{ to: VAULT, data: encodeFunctionData({ abi: VAULT_ABI, functionName, args: fnArgs }) }, 'latest']) });
const mined = async (hash) => {
  for (let i = 0; i < 100; i++) {
    const r = await rpc('eth_getTransactionReceipt', [hash]);
    if (r) return r;
    await delay(300);
  }
  throw new Error(`no receipt for ${hash}`);
};
/** Polls `read` until `ok` or the stated timeout: never a fixed sleep in place of a condition. */
const until = async (what, read, ok, timeoutMs, everyMs = 1500) => {
  const end = Date.now() + timeoutMs;
  let last;
  while (Date.now() < end) {
    last = await read();
    if (ok(last)) return last;
    await delay(everyMs);
  }
  throw new Error(`timeout (${Math.round(timeoutMs / 1000)} s) waiting for ${what}: ${JSON.stringify(last, (k, v) => (typeof v === 'bigint' ? v.toString() : v))?.slice(0, 300)}`);
};

// ---- The stack is what it must be, or nothing is sent.
const client = await rpc('web3_clientVersion');
const chain = await rpc('eth_chainId');
const status = await get('/v1/status');
if (!/^anvil/i.test(client)) fail(`the L2 endpoint is not anvil (${client})`);
if (chain !== '0xab162') fail(`the L2 chain is ${chain}, not 700770`);
if (!status || status.unaudited !== true) fail('the API is not an unaudited development node');
if (REHEARSE) {
  if ((await get('/mock/info'))?.mock !== true) fail('--rehearse is for the reference mock only');
} else {
  if (status.mode !== 'dev') fail('the API is not a dev-mode node');
  if (status.network.vault.toLowerCase() !== VAULT || status.network.collateralToken.toLowerCase() !== TOKEN) fail('the node names another vault or token');
}
const devAccounts = (await rpc('eth_accounts')).map((a) => a.toLowerCase());
const taker = devAccounts[TAKER_INDEX];
const maker = devAccounts[MAKER_INDEX];
if (!taker || !maker || taker === maker) fail('anvil lacks the two development accounts asked for');

const facts = {
  harness: 'stack-write.mjs (the page writes to a real local stack; UNAUDITED prototype)',
  rehearsal: REHEARSE ? 'LOCAL DRY RUN against the reference mock: the node credit and the vault payout are not rehearsed' : false,
  startedAt: new Date().toISOString(),
  app: APP,
  api: API,
  l2: L2,
  l2Client: client,
  chainId: 700770,
  vault: VAULT,
  token: TOKEN,
  vaultAbi: 'config/vault-abi.json (the page\'s pinned ABI)',
  node: { mode: status.mode, apiVersion: status.apiVersion, heightAtStart: status.latestBlock?.height },
  taker,
  maker,
  anvilDevelopmentAccounts: { taker: TAKER_INDEX, maker: MAKER_INDEX },
  keyHeldByThisScript: false,
  engineAccountsBefore: { taker: await get(`/v1/accounts/${taker}`), maker: await get(`/v1/accounts/${maker}`) },
  notRehearsed: [],
};
const results = [];
const failures = [];
const expect = (step, ok, what, detail) => {
  results.push({ step, ok: !!ok, what, ...(detail === undefined ? {} : { detail }) });
  if (!ok) failures.push(`${step}: ${what}`);
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${step}: ${what}`);
};
const skipped = (step, what) => {
  facts.notRehearsed.push(`${step}: ${what}`);
  console.log(`skip ${step}: ${what} (not rehearsed against the mock)`);
};

const walletScript = (address) => String.raw`
(() => {
  const address = ${JSON.stringify(address)};
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

const browser = await launch(CHROME, PORT);
/** One person at the page: an own browser context, an own wallet that forwards to anvil for `account` only. */
async function person(name, account) {
  const sent = [];
  const signed = [];
  const onBinding = async (binding, payload, p) => {
    if (binding !== '__walletBridge') return;
    const m = JSON.parse(payload);
    const refuse = (why) => p.evaluate(`window.__walletResolve(${m.id}, false, ${JSON.stringify(why)})`);
    try {
      if (m.kind === 'tx') {
        const { tx } = m;
        const to = (tx.to ?? '').toLowerCase();
        if ((tx.from ?? '').toLowerCase() !== account || (to !== VAULT && to !== TOKEN) || (tx.value ?? '0x0') !== '0x0') return refuse('refused by the harness wallet');
        // Named by the ABI of the contract it goes to; a call the ABI does not know is REFUSED, not recorded blind.
        let call;
        try {
          call = decodeFunctionData({ abi: to === TOKEN ? TOKEN_ABI : VAULT_ABI, data: tx.data }).functionName;
        } catch {
          return refuse(`refused by the harness wallet: selector ${tx.data.slice(0, 10)} is not in the ${to === TOKEN ? 'token' : 'pinned vault'} ABI`);
        }
        const hash = await rpc('eth_sendTransaction', [tx]);
        sent.push({ to: to === TOKEN ? 'token' : 'vault', call, hash });
        return p.evaluate(`window.__walletResolve(${m.id}, true, ${JSON.stringify(hash)})`);
      }
      const typed = JSON.parse(m.data);
      if (m.from.toLowerCase() !== account || typed.domain?.name !== 'PosternPerps' || Number(typed.domain?.chainId) !== 700770 || (typed.domain?.verifyingContract ?? '').toLowerCase() !== VAULT) return refuse('refused by the harness wallet');
      const signature = await rpc('eth_signTypedData_v4', [account, typed]);
      signed.push({ primaryType: typed.primaryType, message: typed.message });
      return p.evaluate(`window.__walletResolve(${m.id}, true, ${JSON.stringify(signature)})`);
    } catch (e) {
      return refuse(String(e.message ?? e));
    }
  };
  const { browserContextId } = await browser.send('Target.createBrowserContext');
  const page = await openPage(browser, browserContextId, { initScript: walletScript(account), url: 'about:blank', onBinding, log: () => {} });
  await page.cdp('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
  await page.goto(APP);
  await page.evaluate(`localStorage.setItem('postern.theme.v1', 'dark')`);
  const shot = async (file) => {
    await delay(500);
    const { data } = await page.cdp('Page.captureScreenshot', { format: 'png' });
    writeFileSync(join(OUT, `${file}.png`), Buffer.from(data, 'base64'));
  };
  /** Opens an address of the page from a clean load and connects the wallet again: a reload DISCONNECTS the page. */
  const open = async (route, ready) => {
    await page.goto(`${APP}${route}`);
    await page.reload();
    await page.waitFor('the page shell', `!!window.__h.button('Connect') || !!document.querySelector('.wallet-button')`, 30_000);
    if (await page.evaluate(`!!window.__h.button('Connect')`)) {
      await page.click('Connect');
      await page.click('Anvil wallet');
    }
    await page.waitFor('the wallet to be connected', `!window.__h.button('Connect') && !window.__h.button('Connecting')`, 30_000);
    if (ready) await page.waitVisibleText(ready, 40_000);
  };
  const sheetSend = async (title) => {
    await page.waitVisibleText(/Review before sending · Bloch L2 transaction/, 30_000);
    await page.waitVisibleText(title, 15_000);
    await page.click('Send in wallet');
    await page.waitVisibleText(/In L2 block \d+/, 90_000);
    await page.click('Close');
  };
  /** Signs the review sheet of an exchange action and returns the page's words about its fate. */
  const signAction = async () => {
    await page.waitVisibleText(/Review before signing/, 20_000);
    await page.click('Sign in wallet and send');
    await page.waitVisibleText(/Included in block|Rejected|Refused/, 60_000);
    const text = await page.visibleText();
    await page.click('Close').catch(() => {});
    return text;
  };

  // ---- Test money, then the deposit FROM THE PAGE.
  const deposit = async () => {
    const step = `${name} deposit`;
    if ((await balanceOf(account)) < 1_100_000_000n) {
      const mint = await rpc('eth_sendTransaction', [{ from: account, to: TOKEN, data: encodeFunctionData({ abi: TOKEN_ABI, functionName: 'mint', args: [account, 2_000_000_000n] }) }]);
      const receipt = await mined(mint);
      facts[`${name}MintByHarness`] = { hash: mint, block: Number(receipt.blockNumber), status: receipt.status };
      expect(step, receipt.status === '0x1', 'mock bUSDC minted to the account (by the harness, not the page)');
    }
    const tokenBefore = await balanceOf(account);
    const before = Number((await get(`/v1/accounts/${account}`))?.collateral ?? 0);
    await open('#/transfers/deposit', /Vault open/);
    await page.fill('Amount', '1000');
    await shot(`${name}-01-deposit-ready`);
    await page.waitFor('the deposit steps', `!!window.__h.button('Review allowance of 1,000.00 bUSDC') || !!window.__h.button('Review deposit of 1,000.00 bUSDC')`, 40_000);
    if (await page.evaluate(`!!window.__h.button('Review allowance of 1,000.00 bUSDC')`)) {
      await page.click('Review allowance of 1,000.00 bUSDC');
      await sheetSend(/Allow the vault to take 1,000\.00 bUSDC from this wallet/);
    }
    await page.click('Review deposit of 1,000.00 bUSDC', 40_000);
    await page.waitVisibleText(new RegExp(`Credited to trading account\\s*${account}`, 'i'), 20_000);
    await sheetSend(/Deposit 1,000\.00 bUSDC into the exchange vault/);
    await page.waitVisibleText(/Vault deposit #\d+/, 60_000);
    expect(step, (await balanceOf(account)) === tokenBefore - 1_000_000_000n, 'the vault took exactly 1,000.00 bUSDC from the wallet', { before: tokenBefore.toString(), after: (await balanceOf(account)).toString() });
    expect(step, sent.some((t) => t.to === 'vault' && t.call === 'deposit'), 'the deposit transaction was sent by the page (named by the pinned ABI)', sent);
    if (REHEARSE) return skipped(step, 'the node credits the deposit once the L2 block is final');
    const credited = await until('the node to credit the deposit', () => get(`/v1/accounts/${account}`), (a) => !!a && Number(a.collateral) >= before + 999, 300_000).catch((e) => ({ error: String(e.message) }));
    expect(step, !credited.error, 'the node credited the deposit to the trading account', credited.error ?? credited.collateral);
    await shot(`${name}-02-deposit-credited`);
  };

  // ---- The eligibility statement (once per account), then one limit order, both signed in the page.
  const order = async (side, size, limit) => {
    const step = `${name} order`;
    await open('#/trade/BTC-USD', /Before your first order|Time in force|Good till cancelled/);
    if (/Before your first order/.test(await page.visibleText())) {
      await page.fill('Country of residence', 'PA');
      await page.evaluate(`document.querySelectorAll('.attestation input[type=checkbox]').forEach((c) => c.click())`);
      await page.click('Sign the statement');
      await page.waitFor('the statement to be signed', `!document.querySelector('.attestation')`, 40_000);
      expect(step, signed.some((s) => s.primaryType === 'Attestation'), 'the eligibility statement was one typed-data signature asked by the page');
    }
    await page.evaluate(`document.querySelector('.side-switch .side-${side === 'buy' ? 'long' : 'short'}').click()`);
    await page.evaluate(`[...document.querySelectorAll('.mode-tablist [role=tab]')].find((b) => b.textContent.trim() === 'Limit').click()`);
    await page.fill('Size', size);
    await page.fill('Limit price', limit);
    const verb = side === 'buy' ? 'long' : 'short';
    await page.waitVisibleText(new RegExp(`Review ${verb} ${size.replace('.', '\\.')} BTC`), 20_000);
    await shot(`${name}-03-order-form`);
    const signedBefore = signed.length;
    await page.click(`Review ${verb}`);
    await page.waitVisibleText(/Review before signing/, 20_000);
    await shot(`${name}-04-order-review`);
    const text = await signAction();
    expect(step, /Included in block/.test(text), `the ${verb} limit order signed in the page was included`, /Included in block \d+/.exec(text)?.[0] ?? text.slice(0, 300));
    expect(step, signed.length === signedBefore + 1, 'the order was exactly one typed-data signature', signed.slice(signedBefore).map((s) => s.primaryType));
    await shot(`${name}-05-order-included`);
  };

  return { name, account, page, sent, signed, shot, open, sheetSend, signAction, deposit, order };
}

let T;
let M;
try {
  T = await person('taker', taker);
  M = await person('maker', maker);

  // ---- 1. Both accounts deposit from the page.
  await T.deposit();
  await M.deposit();

  // ---- 2. The maker rests a sell just above the mark; the taker's buy at the same price crosses it and FILLS.
  const market = (await get('/v1/markets/BTC-USD')) ?? {};
  const ticker = (await get('/v1/markets/BTC-USD/ticker')) ?? {};
  const mark = Number(ticker.markPrice);
  const tick = Number(market.tickSize);
  if (!(mark > 0) || !(tick > 0)) throw new Error('the node gave no mark price or tick for BTC-USD');
  const decimals = Number(market.priceDecimals ?? 1);
  // 0.2% over the mark: inside every band (mark band 0.5%, market-order band 5%), rounded UP to the tick.
  const limit = (Math.ceil((mark * 1.002) / tick) * tick).toFixed(decimals);
  const size = '0.01';
  facts.order = { mark, tick, limit, size, marketStatus: market.status };
  const fillsBefore = ((await get(`/v1/accounts/${taker}/fills`))?.fills ?? []).length;
  await M.order('sell', size, limit);
  const resting = await until('the maker\'s order to rest in the book', () => get(`/v1/accounts/${maker}/orders`), (o) => (o?.orders ?? []).some((x) => x.symbol === 'BTC-USD' && x.side === 'sell'), 30_000).catch((e) => ({ error: String(e.message) }));
  expect('maker order', !resting.error, 'the node lists the maker\'s resting sell', resting.error ?? (resting.orders ?? []).length);
  await T.order('buy', size, limit);
  const filled = await until('the taker\'s fill', () => get(`/v1/accounts/${taker}/fills`), (f) => (f?.fills ?? []).length > fillsBefore, 30_000).catch((e) => ({ error: String(e.message) }));
  expect('taker order', !filled.error, 'the taker\'s order FILLED: the node lists a new fill of this account', filled.error ?? (filled.fills ?? [])[0]);
  const after = await get(`/v1/accounts/${taker}`);
  expect('taker order', (after?.positions ?? []).some((p) => p.symbol === 'BTC-USD' && Number(p.size) > 0), 'the node shows the taker long BTC-USD', (after?.positions ?? []).map((p) => `${p.symbol} ${p.size}`));
  await T.open('#/trade/BTC-USD', /Positions/);
  await T.shot('taker-06-position');

  // ---- 3. One withdrawal from the page: the signed action, then the payout collected from the vault.
  const { page } = T;
  await T.open('#/transfers/withdraw', /Withdraw to the Bloch L2/);
  const checkpointsBefore = REHEARSE ? 0n : await vaultRead('checkpointCount');
  await page.fill('Amount', '100');
  await T.shot('taker-07-withdraw-form');
  await page.click('Review withdrawal', 30_000);
  await page.waitVisibleText(/Review before signing/, 20_000);
  await T.shot('taker-08-withdraw-review');
  const wText = await T.signAction();
  expect('withdraw', /Included in block/.test(wText), 'the withdrawal signed in the page was included', /Included in block \d+/.exec(wText)?.[0] ?? wText.slice(0, 300));
  expect('withdraw', T.signed.some((s) => s.primaryType === 'Withdraw'), 'the withdrawal was a typed-data signature asked by the page', T.signed.map((s) => s.primaryType));
  if (REHEARSE) skipped('withdraw', 'the checkpoint is relayed, the vault accepts it and pays the withdrawal through the page');
  else {
    const walletBefore = await balanceOf(taker);
    const end = Date.now() + WITHDRAW_TIMEOUT_MS;
    // The exchange certifies the withdrawal in a checkpoint; the relayer delivers it; the VAULT must accept it. The
    // wait is on the vault's own state (checkpointCount by the pinned ABI) and on the node's certificate, with the
    // stated timeout; the page's own check against the vault decides whether the payout can be asked.
    const certified = await until('the node to certify the withdrawal', () => get(`/v1/accounts/${taker}/withdrawals`), (w) => (w?.withdrawals ?? []).some((x) => x.certificate), WITHDRAW_TIMEOUT_MS).catch((e) => ({ error: String(e.message) }));
    expect('withdraw', !certified.error, 'the node certified the withdrawal in a checkpoint', certified.error);
    let asked = false;
    let lastWords = '';
    while (!asked && !certified.error && Date.now() < end) {
      await T.open('#/transfers/withdraw', /Your withdrawals/);
      const offered = await page.waitFor('the collection to be offered', `(() => { const b = window.__h.button('Collect from the vault'); return !!b && !b.disabled; })()`, 20_000).then(() => true, () => false);
      if (offered) {
        await page.click('Collect from the vault');
        // The page checks the certificate against the vault: a review sheet when the vault holds the checkpoint,
        // words when it does not yet.
        asked = await page.waitVisibleText(/Review before sending · Bloch L2 transaction/, 20_000).then(() => true, () => false);
        if (!asked) lastWords = (await page.visibleText()).slice(-400);
      }
      if (!asked) {
        // Not yet at the vault: wait until the vault itself has accepted one more checkpoint, then ask again.
        const count = await vaultRead('checkpointCount');
        await until('the vault to accept another checkpoint', () => vaultRead('checkpointCount'), (c) => c > count, Math.max(1000, Math.min(180_000, end - Date.now()))).catch(() => null);
      }
    }
    facts.vaultCheckpoints = { before: checkpointsBefore.toString(), atCollection: (await vaultRead('checkpointCount')).toString() };
    expect('withdraw', asked, `the page built the vault payout once the vault held the checkpoint (within ${WITHDRAW_TIMEOUT_MS / 1000} s)`, asked ? undefined : lastWords);
    if (asked) {
      await T.shot('taker-09-withdraw-sheet');
      await page.click('Send in wallet');
      await page.waitVisibleText(/In L2 block \d+/, 90_000);
      await page.click('Close');
      const paid = await until('the vault payout', () => balanceOf(taker).then(String), (b) => BigInt(b) > walletBefore, 60_000).catch((e) => ({ error: String(e.message) }));
      facts.payout = { walletBefore: walletBefore.toString(), walletAfter: paid.error ? null : paid };
      expect('withdraw', !paid.error, 'the vault paid the withdrawal to the wallet on the L2', facts.payout);
      expect('withdraw', T.sent.some((t) => t.to === 'vault' && t.call === 'withdraw'), 'the vault transaction was sent by the page (named by the pinned ABI)', T.sent);
      await T.shot('taker-10-withdraw-paid');
    }
  }

  // ---- 4. Nothing of the harness is left resting in the book.
  for (const P of [M, T]) {
    const open = (await get(`/v1/accounts/${P.account}/orders`))?.orders ?? [];
    if (!open.length) continue;
    await P.open('#/trade/BTC-USD', /Open orders/);
    await P.page.evaluate(`[...document.querySelectorAll('.account-tables [role=tab]')].find((b) => b.textContent.startsWith('Open orders')).click()`);
    await P.page.click('Cancel all', 20_000);
    await P.signAction();
  }
  const left = [...(((await get(`/v1/accounts/${maker}/orders`))?.orders) ?? []), ...(((await get(`/v1/accounts/${taker}/orders`))?.orders) ?? [])];
  expect('cancel', left.length === 0, 'no order of the two harness accounts is left in the book', left.length);
  for (const P of [T, M]) {
    const errors = P.page.console.filter((l) => /^(error|pageerror)/.test(l) && !/Failed to load resource|WebSocket/.test(l));
    expect('console', errors.length === 0, `no error in the ${P.name}'s page console`, errors.slice(0, 5));
  }
} catch (e) {
  expect('run', false, `stopped: ${String(e.message ?? e).slice(0, 500)}`);
  for (const P of [T, M]) if (P) await P.shot(`${P.name}-99-stopped`).catch(() => {});
} finally {
  facts.finishedAt = new Date().toISOString();
  facts.engineAccountsAtEnd = { taker: await get(`/v1/accounts/${taker}`).catch(() => null), maker: await get(`/v1/accounts/${maker}`).catch(() => null) };
  facts.transactionsThePageSent = { taker: T?.sent ?? [], maker: M?.sent ?? [] };
  facts.typedDataThePageSigned = { taker: T?.signed ?? [], maker: M?.signed ?? [] };
  writeFileSync(join(OUT, 'results.json'), JSON.stringify({ facts, results, failures }, (k, v) => (typeof v === 'bigint' ? v.toString() : v), 1) + '\n');
  await browser.close();
}
console.log(`${results.length} assertions, ${failures.length} failed${REHEARSE ? `, ${facts.notRehearsed.length} step(s) not rehearsed` : ''}; taker ${taker}, maker ${maker}`);
process.exit(failures.length ? 1 : 0);
