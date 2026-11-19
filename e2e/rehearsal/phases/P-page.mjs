// Phase P: the PAGE writes to the real stack. 434's harness frontend/integration/browser/stack-write.mjs (run
// unchanged from the stack's tree) drives the trading page in headless Chrome: deposit, a limit order, a withdrawal
// collected from the vault, cancel; the harness's second account (also driven from the page) is the counterparty.
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
  let R = null, account = null, out = null, exit = null, log = '';
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
    await Promise.race([done, sleep(1200000)]);
    if (exit === null) { child.kill(); throw new Error(`stack-write.mjs did not finish in 20 minutes; last output: ${log.slice(-400)}`); }
    expect(existsSync(join(out, 'results.json')), `no results.json; exit ${exit}; output: ${log.slice(-600)}`);
    R = JSON.parse(readFileSync(join(out, 'results.json'), 'utf8'));
    return { exit, account, assertions: R.results.length, failed: R.failures, evidence: out };
  });
  const step = (word) => R.results.filter((x) => x.step.includes(word));
  const allOk = (prefix) => { const s = step(prefix); expect(s.length > 0, `the harness recorded no "${prefix}" assertion (it stopped before: ${R.failures.join(' | ').slice(0, 300)})`); expect(s.every((x) => x.ok), s.filter((x) => !x.ok).map((x) => `${x.what}: ${JSON.stringify(x.detail ?? '').slice(0, 200)}`).join(' | ')); return s.map((x) => x.what); };
  await ctx.check('P-02', 'deposit from the page: the vault took the tokens in a transaction the page sent, and the node credited the account', async () => { blockedIf(!R, 'P-01 did not produce results'); return allOk('deposit'); });
  await ctx.check('P-03', 'order from the page: one typed-data signature asked by the page, included by the real node', async () => { blockedIf(!R, 'P-01 did not produce results'); return allOk('order'); });
  await ctx.check('P-04', 'the order placed from the page FILLED (against the harness\'s second account, itself driven from the page): the node lists the fill and both positions, equal and opposite', async () => {
    blockedIf(!R, 'P-01 did not produce results');
    const accts = await chain.rpc('eth_accounts');
    const taker = accts[9].toLowerCase(), mk = accts[8].toLowerCase();
    const f = (await api.json(`/v1/accounts/${taker}/fills?limit=5`)).fills.find((x) => x.symbol === 'BTC-USD' && x.side === 'buy');
    expect(f, 'the node lists no buy fill for the page\'s taker account');
    const fm = (await api.json(`/v1/accounts/${mk}/fills?limit=5`)).fills.find((x) => x.tradeId === f.tradeId);
    expect(fm && fm.side === 'sell' && fm.liquidity === 'maker' && f.liquidity === 'taker', `maker fill ${JSON.stringify(fm)}`);
    const pt = (await api.account(taker)).positions.find((x) => x.symbol === 'BTC-USD'), pm = (await api.account(mk)).positions.find((x) => x.symbol === 'BTC-USD');
    expect(pt && pm && Number(pt.size) === Number(f.size) && Number(pm.size) === -Number(f.size), `positions ${JSON.stringify([pt?.size, pm?.size])}`);
    return { fill: f, makerFill: fm, positions: { taker: pt.size, maker: pm.size } };
  });
  await ctx.check('P-05', 'withdrawal from the page: signed in the page, included by the node, collected from the vault in a transaction the page sent, paid on the L2', async () => { blockedIf(!R, 'P-01 did not produce results'); return allOk('withdraw'); });
  await ctx.check('P-06', 'the harness ends clean: nothing left in the book, no error in the page console, every one of its assertions passed', async () => {
    blockedIf(!R, 'P-01 did not produce results');
    expect(R.failures.length === 0 && exit === 0, `exit ${exit}; failed: ${R.failures.join(' | ').slice(0, 500)}`);
    // the harness's own summary, as it wrote it (its shape is the harness's: copied, not interpreted)
    return { assertions: R.results.length, transactionsThePageSent: R.facts.transactionsThePageSent ?? null, typedDataThePageSigned: R.facts.typedDataThePageSigned ?? null };
  });
}
