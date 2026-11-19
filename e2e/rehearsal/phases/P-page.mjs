// Phase P: the PAGE writes to the real stack. 434's harness frontend/integration/browser/stack-write.mjs (run
// unchanged from the stack's tree) drives the trading page in headless Chrome: deposit, a limit order, a withdrawal
// collected from the vault, cancel; meanwhile the rehearsal's maker sells into the order the page placed, so it FILLS.
// No key is used: anvil signs for one of its own development accounts. UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { sleep } from '../../lib/client.mjs';
import { spawn } from 'node:child_process';
import { existsSync, readFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

export default async function (ctx) {
  const { api, stack, chain } = ctx;
  const btc = api.market('BTC-USD');
  const maker = ctx.who('maker');
  let R = null, account = null, filled = null, out = null, exit = null, log = '';
  await ctx.check('P-01', 'the page (headless Chrome, 434\'s stack-write.mjs, unchanged) deposits, places a limit order, withdraws and collects from the vault: the harness finishes', async () => {
    blockedIf(!stack.frontend || !chain, 'the stack runs without the frontend dev server (up.sh --frontend)');
    const script = join(stack.tree, 'frontend', 'integration', 'browser', 'stack-write.mjs');
    blockedIf(!existsSync(script), 'this tree has no frontend/integration/browser/stack-write.mjs');
    out = join(process.env.HOME, 'Desktop/POSTERNDEX-PERPS-20261117/agents/435-evidence', `page-write-${new Date().toISOString().replace(/[-:]/g, '').slice(0, 15)}Z`);
    mkdirSync(out, { recursive: true });
    account = (await chain.rpc('eth_accounts'))[9].toLowerCase();
    await api.cancelAll(maker);
    const child = spawn(process.execPath, [script, '--app', `${stack.frontend}/`, '--api', stack.api, '--l2', stack.l2Rpc, '--vault', stack.vault, '--token', stack.token, '--out', out, '--port', '49955'], { cwd: join(stack.tree, 'frontend') });
    child.stdout.on('data', (d) => (log += d)); child.stderr.on('data', (d) => (log += d));
    const done = new Promise((res) => child.on('close', (code) => { exit = code; res(); }));
    // the counterparty: once the page's bid rests AND the page has moved on to its withdrawal, sell into the bid
    (async () => {
      for (let i = 0; i < 600 && exit === null; i++) {
        await sleep(1000);
        try {
          const o = (await api.json(`/v1/accounts/${account}/orders?status=open`)).orders.find((x) => x.symbol === 'BTC-USD' && x.side === 'buy');
          const w = (await api.json(`/v1/accounts/${account}/withdrawals`)).withdrawals;
          if (o && w.length > 0) {
            const r = await api.order(maker, btc, { buy: false, price: o.price, size: o.size, tif: 1 });
            filled = { order: o, makerResult: r.body?.result };
            return;
          }
        } catch { /* keep polling */ }
      }
    })();
    await Promise.race([done, sleep(600000)]);
    if (exit === null) { child.kill(); throw new Error(`stack-write.mjs did not finish in 10 minutes; last output: ${log.slice(-400)}`); }
    expect(existsSync(join(out, 'results.json')), `no results.json; exit ${exit}; output: ${log.slice(-600)}`);
    R = JSON.parse(readFileSync(join(out, 'results.json'), 'utf8'));
    return { exit, account, assertions: R.results.length, failed: R.failures, evidence: out };
  });
  const step = (prefix) => R.results.filter((x) => x.step.startsWith(prefix));
  const allOk = (prefix) => { const s = step(prefix); expect(s.length > 0, `the harness recorded no "${prefix}" assertion (it stopped before: ${R.failures.join(' | ').slice(0, 300)})`); expect(s.every((x) => x.ok), s.filter((x) => !x.ok).map((x) => `${x.what}: ${JSON.stringify(x.detail ?? '').slice(0, 200)}`).join(' | ')); return s.map((x) => x.what); };
  await ctx.check('P-02', 'deposit from the page: the vault took the tokens in a transaction the page sent, and the node credited the account', async () => { blockedIf(!R, 'P-01 did not produce results'); return allOk('1 deposit'); });
  await ctx.check('P-03', 'order from the page: one typed-data signature asked by the page, included by the real node', async () => { blockedIf(!R, 'P-01 did not produce results'); return allOk('2 order'); });
  await ctx.check('P-04', 'the order placed from the page FILLS against the rehearsal\'s maker: the page\'s account holds the position and the fill', async () => {
    blockedIf(!R, 'P-01 did not produce results');
    expect(filled && filled.makerResult?.ok === true, `the maker did not get to sell into the page's order: ${JSON.stringify(filled)}`);
    const f = (await api.json(`/v1/accounts/${account}/fills?limit=5`)).fills.find((x) => x.orderId === filled.order.orderId);
    expect(f && f.side === 'buy' && f.liquidity === 'maker' && Number(f.price) === Number(filled.order.price) && Number(f.size) === Number(filled.order.size), `fill ${JSON.stringify(f)}`);
    const p = (await api.account(account)).positions.find((x) => x.symbol === 'BTC-USD');
    expect(p && Number(p.size) === Number(filled.order.size), `position ${JSON.stringify(p)}`);
    return { order: filled.order, fill: f, position: { size: p.size, entryPrice: p.entryPrice } };
  });
  await ctx.check('P-05', 'withdrawal from the page: signed in the page, included by the node, collected from the vault in a transaction the page sent, paid on the L2', async () => { blockedIf(!R, 'P-01 did not produce results'); return allOk('3 withdraw'); });
  await ctx.check('P-06', 'the harness ends clean: nothing left in the book, no error in the page console, every one of its assertions passed', async () => {
    blockedIf(!R, 'P-01 did not produce results');
    expect(R.failures.length === 0 && exit === 0, `exit ${exit}; failed: ${R.failures.join(' | ').slice(0, 500)}`);
    return { assertions: R.results.length, transactionsThePageSent: R.facts.transactionsThePageSent?.map((t) => `${t.to}.${t.call}`), typedDataThePageSigned: R.facts.typedDataThePageSigned?.map((t) => t.primaryType) };
  });
}
