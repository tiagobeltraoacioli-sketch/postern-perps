/**
 * The Funds page IN A BROWSER against the real PerpsVault bytecode on a local anvil (chain 700770): a user deposits,
 * posts a forced request the exchange ignores, the vault goes to escape, and the user gets everything back through
 * the screens alone. UNAUDITED prototype; loopback only; committee keys generated at run time.
 *
 * Cast: anvil with the vault (integration/vault/stand-in.ts); the reference mock (node/mock) as the exchange API for
 * market data only: it knows nothing of this vault, which is exactly the "exchange is silent" case the exit exists
 * for; the page from `scripts/dev-local.mjs`; headless Chrome driven over CDP. The wallet injected into the page is
 * anvil's public development account 1, unlocked on the node (no key is read): every `eth_sendTransaction` the page
 * hands it is recorded here and forwarded to anvil. Run through integration/vault/run.mjs (set PERPS_BROWSER=1).
 */
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { decodeFunctionData, type Hex } from 'viem';
// @ts-expect-error plain ES module without types (the CDP helper of the browser harness)
import { launch, openPage } from '../browser/cdp.mjs';
import { TOKEN_ABI, VAULT_ABI } from '../../src/vault/abi';
import { keyless, standIn, U, ZERO32, type StandIn } from './stand-in';

const RPC = process.env.PERPS_ANVIL_RPC ?? '';
const OUT = process.env.PERPS_FORGE_OUT ?? '';
const SHOTS = process.env.PERPS_SHOTS ?? '';
const ON = !!RPC && !!OUT && process.env.PERPS_BROWSER === '1';
const MOCK_PORT = 49815;
const APP_PORT = 49816;
const CDP_PORT = 49852;
const APP = `http://127.0.0.1:${APP_PORT}/`;
const CHROME = process.env.PERPS_CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

type Page = {
  goto(url: string): Promise<void>;
  reload(): Promise<void>;
  evaluate(expression: string): Promise<unknown>;
  click(name: string, timeoutMs?: number): Promise<void>;
  fill(label: string, value: string): Promise<void>;
  waitVisibleText(pattern: RegExp, timeoutMs?: number): Promise<unknown>;
  waitFor(what: string, expression: string, timeoutMs?: number): Promise<unknown>;
  visibleText(): Promise<string>;
  screenshot(file: string): Promise<void>;
  cdp(method: string, params?: object): Promise<{ data: string }>;
  console: string[];
};

let s: StandIn;
let user: Hex;
let relayer: Hex;
let page: Page;
let browser: { close(): Promise<void>; send(method: string, params?: object): Promise<{ browserContextId: string }> };
const children: ChildProcess[] = [];
/** Every transaction the page handed to the wallet: the method names of the vault and token calls, in order. */
const sent: string[] = [];

const shot = async (name: string) => {
  if (!SHOTS) return;
  await delay(500);
  await page.screenshot(join(SHOTS, `${name}.png`));
};
const sheetSend = async (title: RegExp) => {
  await page.waitVisibleText(/Review before sending · Bloch L2 transaction/, 20_000);
  await page.waitVisibleText(title, 10_000);
  await page.click('Send in wallet');
  await page.waitVisibleText(/In L2 block \d+/, 60_000);
  await page.click('Close');
};
const pasteInto = (label: string, text: string) =>
  page.evaluate(
    `(() => { const l = [...document.querySelectorAll('label')].find((x) => x.textContent.trim().startsWith(${JSON.stringify(label)})); const t = l.querySelector('textarea'); Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(t, ${JSON.stringify(text)}); t.dispatchEvent(new Event('input', { bubbles: true })); })()`,
  );
const openDetails = (summary: string) => page.evaluate(`(() => { const d = [...document.querySelectorAll('details')].find((x) => x.querySelector('summary')?.textContent.trim().startsWith(${JSON.stringify(summary)})); if (!d) throw new Error('no details ' + ${JSON.stringify(summary)}); d.open = true; })()`);
const up = async (url: string, what: string) => {
  for (let i = 0; i < 150; i++) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      /* not yet */
    }
    await delay(200);
  }
  throw new Error(`${what} did not start`);
};

describe.skipIf(!ON)('Funds page in a browser against PerpsVault on anvil', () => {
  beforeAll(async () => {
    if (SHOTS) mkdirSync(SHOTS, { recursive: true });
    s = await standIn(RPC, OUT);
    [, user, , relayer] = s.accounts;
    await s.mint(user, 10_000n * U);
    const frontend = process.cwd();
    const node = process.execPath;
    children.push(spawn(node, [resolve(frontend, '../node/mock/server.mjs'), '--port', String(MOCK_PORT), '--start-time', 'now'], { stdio: 'ignore' }));
    await up(`http://127.0.0.1:${MOCK_PORT}/v1/status`, 'the mock API');
    await fetch(`http://127.0.0.1:${MOCK_PORT}/mock/scenario?geo=ZZ`);
    children.push(
      spawn(node, ['scripts/dev-local.mjs', '--api', `http://127.0.0.1:${MOCK_PORT}`, '--l2', RPC, '--vault', s.deployment.vault, '--token', s.deployment.token, '--port', String(APP_PORT), '--config-out', resolve(frontend, '.local/vault-browser.json')], {
        cwd: frontend,
        stdio: 'ignore',
      }),
    );
    await up(APP, 'the dev server');
    browser = await launch(CHROME, CDP_PORT);
    const { browserContextId } = await browser.send('Target.createBrowserContext');
    const initScript = String.raw`
(() => {
  const address = ${JSON.stringify(user)};
  const waiting = new Map();
  let next = 0;
  window.__walletResolve = (id, ok, value) => { const w = waiting.get(id); waiting.delete(id); if (w) ok ? w.resolve(value) : w.reject(Object.assign(new Error(value), { code: 4001 })); };
  const listeners = {};
  const provider = {
    request: async ({ method, params }) => {
      if (method === 'eth_requestAccounts' || method === 'eth_accounts') return [address];
      if (method === 'eth_chainId') return '0xab162';
      if (method === 'eth_sendTransaction') {
        const id = ++next;
        const p = new Promise((resolve, reject) => waiting.set(id, { resolve, reject }));
        window.__walletBridge(JSON.stringify({ id, tx: params[0] }));
        return p;
      }
      throw Object.assign(new Error('this harness wallet signs nothing'), { code: 4001 });
    },
    on: (e, f) => ((listeners[e] ||= []).push(f)),
    removeListener: (e, f) => (listeners[e] = (listeners[e] || []).filter((x) => x !== f)),
  };
  const info = { uuid: '8b3e4d5c-6f70-4b82-8c9d-1e2f3a4b5c6d', name: 'Anvil wallet', icon: '', rdns: 'local.anvil' };
  const announce = () => window.dispatchEvent(new CustomEvent('eip6963:announceProvider', { detail: Object.freeze({ info, provider }) }));
  window.addEventListener('eip6963:requestProvider', announce);
  announce();
})();`;
    const onBinding = async (name: string, payload: string, p: Page) => {
      if (name !== '__walletBridge') return;
      const { id, tx } = JSON.parse(payload) as { id: number; tx: { from: Hex; to: Hex; data: Hex; value: string; gas: string } };
      // The wallet's own rules: only this account, only these two contracts, no value.
      const to = tx.to.toLowerCase();
      if (tx.from.toLowerCase() !== user || (to !== s.deployment.vault && to !== s.deployment.token) || tx.value !== '0x0' || !/^0x[0-9a-f]+$/.test(tx.gas)) {
        await p.evaluate(`window.__walletResolve(${id}, false, 'refused by the harness wallet')`);
        return;
      }
      sent.push(decodeFunctionData({ abi: to === s.deployment.token ? TOKEN_ABI : VAULT_ABI, data: tx.data }).functionName);
      const hash = await s.rpc('eth_sendTransaction', [tx]);
      await p.evaluate(`window.__walletResolve(${id}, true, ${JSON.stringify(hash)})`);
    };
    page = (await openPage(browser, browserContextId, { initScript, url: 'about:blank', onBinding, log: () => {} })) as Page;
    await page.cdp('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
    await page.goto(APP);
    await page.evaluate(`localStorage.setItem('postern.theme.v1', 'dark')`);
  });

  afterAll(async () => {
    await browser?.close().catch(() => {});
    for (const c of children) c.kill('SIGTERM');
  });

  it('deposit: exact allowance, then the deposit, then the vault\'s own record and the credit', async () => {
    await page.goto(`${APP}#/transfers/deposit`);
    await page.reload();
    await page.click('Connect');
    await page.click('Anvil wallet');
    await page.waitVisibleText(/Vault open/, 30_000);
    await page.waitVisibleText(/In this wallet: 10,000\.00 bUSDC/, 30_000);
    await page.fill('Amount', '5');
    await page.waitVisibleText(/The vault's minimum deposit is 10\.00 bUSDC/);
    await page.fill('Amount', '1000');
    await shot('f01-deposit-ready');
    await page.click('Review allowance of 1,000.00 bUSDC');
    await page.waitVisibleText(/never unlimited/);
    await shot('f02-allowance-sheet');
    await sheetSend(/Allow the vault to take 1,000\.00 bUSDC from this wallet/);
    await page.click('Review deposit of 1,000.00 bUSDC', 30_000);
    await page.waitVisibleText(new RegExp(`Credited to trading account\\s*${user}`));
    await shot('f03-deposit-sheet');
    await sheetSend(/Deposit 1,000\.00 bUSDC into the exchange vault/);
    await page.waitVisibleText(/Vault deposit #0/, 40_000);
    expect(await page.visibleText()).toMatch(/1,000\.00 bUSDC\s*(Pending|L2 final)/);
    expect(await s.balanceOf(user)).toBe(9_000n * U);
    // The exchange credits it: a checkpoint signed by the committee acknowledges deposit 0.
    s.state.leaves = [
      { account: keyless(1), index: 0, balance: 0n, escapeValue: 0n, positionsHash: ZERO32 },
      { account: keyless(2), index: 1, balance: 60n * U, escapeValue: 60n * U, positionsHash: ZERO32 },
      { account: keyless(3), index: 2, balance: 25n * U, escapeValue: 25n * U, positionsHash: ZERO32 },
      { account: user, index: 3, balance: 915n * U, escapeValue: 915n * U, positionsHash: ZERO32 },
    ];
    await s.checkpoint(1n, 0n, relayer);
    await page.waitVisibleText(/1,000\.00 bUSDC\s*Credited/, 40_000);
    await shot('f04-deposit-credited');
    expect(sent).toEqual(['approve', 'deposit']);
  });

  it('a second deposit the exchange never credits, and a forced request it ignores', async () => {
    await page.fill('Amount', '100');
    await page.click('Review allowance of 100.00 bUSDC', 30_000);
    await sheetSend(/Allow the vault to take 100\.00 bUSDC/);
    await page.click('Review deposit of 100.00 bUSDC', 30_000);
    await sheetSend(/Deposit 100\.00 bUSDC into the exchange vault/);
    await page.waitVisibleText(/Vault deposit #1/, 40_000);
    await page.goto(`${APP}#/transfers/exit`);
    await page.waitVisibleText(/Your funds can always leave, even if the exchange is silent/, 20_000);
    await page.waitVisibleText(/Escape is not due\. The exchange is within its deadlines\./, 30_000);
    await page.waitVisibleText(/The vault gives the exchange 2 d from the request's L2 block/);
    await shot('f05-exit-normal');
    await page.click('Review request');
    await page.waitVisibleText(/All free collateral/);
    await shot('f06-forced-sheet');
    await sheetSend(/Post a request on the L2 the exchange must process: withdraw from my trading account/);
    await page.waitVisibleText(/Request #0 is open/, 40_000);
    await page.waitVisibleText(/The exchange must process it by \d{4}-\d\d-\d\d \d\d:\d\d UTC \(in (1 d 23 h|2 d)/);
    await shot('f07-forced-open');
    expect(sent.slice(2)).toEqual(['approve', 'deposit', 'requestForced']);
  });

  it('the deadline passes: the page offers escape, the system proof, the claim from a pasted file, and the refund', async () => {
    await s.warp(2n * 86_400n + 1n);
    await page.waitVisibleText(/The exchange is overdue/, 40_000);
    await page.waitVisibleText(/A request on the L2 passed its deadline without being processed/);
    await page.waitVisibleText(/One way\./);
    await shot('f08-escape-due');
    await page.click('Review: start escape mode');
    await page.waitVisibleText(/ONE WAY: the exchange stops for good/);
    await shot('f09-escape-sheet');
    await sheetSend(/Switch the vault to escape mode/);
    await page.waitVisibleText(/Escape mode since/, 40_000);
    await page.waitVisibleText(/First, once for everyone: prove the insurance and backstop balances/);
    // The exchange API (the mock) has no proof for this vault: the user brings a proof file, as from any validator.
    const tree = s.accountTree();
    const data = (i: number) => ({ leafData: { account: s.state.leaves[i].account, index: i, balance: s.state.leaves[i].balance.toString(), escapeValue: s.state.leaves[i].escapeValue.toString(), positionsHash: ZERO32 }, proof: tree.proof(i) });
    const file = JSON.stringify({ ...data(3), system: { insurance: data(1), backstop: data(2) } });
    await openDetails('Paste a proof file that holds the two system proofs');
    await pasteInto('Proof file (JSON)', file);
    await shot('f10-escape-unproven');
    await page.click('Review the system proof');
    await sheetSend(/Prove the insurance and backstop balances to the vault/);
    await page.waitVisibleText(/Claim your last proven balance/, 40_000);
    // A proof for another root is refused in words before the wallet is asked.
    await openDetails('The API is down: paste a proof');
    await pasteInto('Account proof (JSON)', JSON.stringify({ ...data(3), leafData: { ...data(3).leafData, escapeValue: '916000000' } }));
    await page.click('Check this proof');
    await page.waitVisibleText(/The proof does not match the last checkpoint the vault accepted/);
    await pasteInto('Account proof (JSON)', file);
    await page.click('Check this proof');
    await page.waitVisibleText(/The vault will pay\s*915\.00 bUSDC/);
    await shot('f11-escape-claim');
    await page.click('Review claim');
    await sheetSend(new RegExp(`Claim the escape balance of ${user}`));
    await page.waitVisibleText(/Your escape balance was paid/, 40_000);
    await page.waitVisibleText(/Deposits the exchange never credited/);
    await page.click('Review refund');
    await sheetSend(new RegExp(`Refund deposit #1: 100\\.00 bUSDC to ${user}`));
    await page.waitVisibleText(/Refunded/, 40_000);
    await shot('f12-escape-done');
    expect(sent.slice(5)).toEqual(['triggerEscape', 'proveSystemShares', 'claimEscape', 'refundDeposit']);
    // 10,000 - 1,000 - 100 deposited + 915 claimed + 100 refunded: the 85 difference is what the stand-in engine
    // moved to the insurance fund and the backstop (their escape values), exactly.
    expect(await s.balanceOf(user)).toBe(9_915n * U);
    const errors = page.console.filter((l) => /^(error|pageerror)/.test(l) && !/Failed to load resource|WebSocket/.test(l));
    expect(errors).toEqual([]);
    if (SHOTS) writeFileSync(join(SHOTS, 'results.json'), `${JSON.stringify({ suite: 'Funds page in a browser vs PerpsVault on anvil (UNAUDITED prototype)', transactionsThePageSent: sent, userBalanceAtEnd: (await s.balanceOf(user)).toString() }, null, 1)}\n`);
  });
});
