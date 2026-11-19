// Phase B: deposits through the real vault, credited only after L2 finality and only when both endpoints agree.
// UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { usdc, sleep } from '../../lib/client.mjs';
import { bal, fmtUsdc } from '../util.mjs';

async function deposit(ctx, address, amount) {
  const { chain, stack } = ctx;
  const units = String(usdc(amount));
  let r = await chain.send(address, stack.token, 'mint(address,uint256)', [address, units]);
  expect(r.ok, `mint: ${r.error}`);
  r = await chain.send(address, stack.token, 'approve(address,uint256)', [stack.vault, units]);
  expect(r.ok, `approve: ${r.error}`);
  r = await chain.send(address, stack.vault, 'deposit(uint64,address)', [units, address]);
  expect(r.ok, `deposit: ${r.error}`);
  return r;
}

export default async function (ctx) {
  const { api, stack, chain, CAST } = ctx;
  const fake = !chain;
  let last = null;
  await ctx.check('B-01', 'every rehearsal account approves and deposits bUSDC into the vault (L2 transactions)', async () => {
    blockedIf(fake, 'fake L2 stand-in: deposits are fixed events, no token and no vault');
    const txs = {};
    for (const a of CAST) { last = await deposit(ctx, a.address, a.deposit); txs[a.label] = { tx: last.tx, block: last.block, gas: last.gas }; }
    const count = Number(await chain.callOne(stack.vault, 'depositCount()(uint64)'));
    expect(count === CAST.length, `vault depositCount ${count}`);
    return txs;
  });
  await ctx.check('B-02', 'a deposit in an L2 block that is not yet final is NOT credited', async () => {
    blockedIf(fake, 'fake L2 stand-in: its chain is final from the start');
    expect(last, 'no deposit made');
    const a = CAST.at(-1);
    const fin = await chain.block('finalized');
    const c = await bal(api, a.address);
    expect(fin.number < last.block, `the deposit block ${last.block} was already final (${fin.number}) when read: nothing observed`);
    expect(c === 0n, `credited ${fmtUsdc(c)} while L2 finalized ${fin.number} < deposit block ${last.block}`);
    return { depositBlock: last.block, finalizedWhenRead: fin.number, collateralWhenRead: fmtUsdc(c) };
  });
  await ctx.check('B-03', 'after L2 finality every account is credited exactly what it deposited', async () => {
    if (last) await chain.waitFinalized(last.block);
    const out = {};
    for (const a of CAST) {
      const want = usdc(a.deposit);
      await api.until(async () => (await bal(api, a.address)) >= want, { ms: 60000, what: `credit of ${a.label}` });
      const got = await bal(api, a.address);
      expect(got === want, `${a.label}: ${fmtUsdc(got)} credited, ${a.deposit} deposited`);
      out[a.label] = fmtUsdc(got);
    }
    return out;
  });
  await ctx.check('B-04', 'vault custody equals the sum of the deposits; the engine credited the same count', async () => {
    blockedIf(fake, 'fake L2 stand-in: no vault');
    const custody = BigInt(await chain.callOne(stack.vault, 'custody()(uint64)'));
    const sum = CAST.reduce((s, a) => s + usdc(a.deposit), 0n);
    expect(custody === sum, `custody ${fmtUsdc(custody)} vs deposits ${fmtUsdc(sum)}`);
    const tokenBal = BigInt(await chain.callOne(stack.token, 'balanceOf(address)(uint256)', [stack.vault]));
    expect(tokenBal === sum, `token balance ${tokenBal}`);
    return { custody: fmtUsdc(custody), deposits: CAST.length };
  });
  for (const [id, mode, what] of [['B-05', 'amount', 'shows a different deposit amount'], ['B-06', 'drop', 'hides the deposit'], ['B-07', 'hash', 'reports another block hash']]) {
    await ctx.check(id, `one L2 endpoint ${what}: the deposit is not credited until the endpoints agree again, then exactly once`, async () => {
      blockedIf(fake, 'fake L2 stand-in: no second endpoint to corrupt');
      const a = ctx.who('alice');
      const before = await bal(api, a.address);
      const set = async (m) => { const r = await fetch(`${stack.l2Proxy}/__fault`, { method: 'POST', body: JSON.stringify({ mode: m }) }); expect(r.status === 200, `proxy fault ${m}: HTTP ${r.status}`); return r.json(); };
      await set(mode);
      let during, node, altered;
      try {
        const d = await deposit(ctx, a.address, '1000');
        await chain.waitFinalized(d.block);
        await sleep(6000);   // several watcher polls after finality
        during = await bal(api, a.address);
        node = (await api.json('/v1/node')).l2;
        altered = (await (await fetch(`${stack.l2Proxy}/__fault`)).json()).altered;
      } finally {
        await set('none');
      }
      expect(during === before, `credited ${fmtUsdc(during - before)} while one endpoint lied`);
      expect(altered > 0 || mode === 'hash', 'the proxy altered nothing: the fault was not exercised');
      await api.until(async () => (await bal(api, a.address)) > before, { ms: 60000, what: 'credit after the endpoints agree' });
      await sleep(3000);
      const after = await bal(api, a.address);
      expect(after === before + usdc('1000'), `after: ${fmtUsdc(after)}, expected ${fmtUsdc(before + usdc('1000'))}`);
      a.deposit = String(BigInt(a.deposit) + 1000n);
      return { before: fmtUsdc(before), whileLying: fmtUsdc(during), after: fmtUsdc(after), watcherWhileLying: { endpointsAgree: node.endpointsAgree, lastError: node.lastError, disagreements: node.disagreements } };
    });
  }
}
