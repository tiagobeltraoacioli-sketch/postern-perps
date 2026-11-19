// Phase H: a user the sequencer ignores leaves through the vault: forced cancel, forced close, forced withdrawal.
// From H-01 on the account sends NOTHING to the API; every request is an L2 transaction. UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { usdc, sleep } from '../../lib/client.mjs';
import { bal, fmtUsdc, onTick } from '../util.mjs';
import { user } from '../../lib/keys.mjs';
import { relayLatest } from './F-finality.mjs';
import { W_SIG, recordTuple, provenWithdrawal } from './G-withdraw.mjs';

export default async function (ctx) {
  const { api, chain, stack } = ctx;
  if (!chain) { await ctx.blocked('H-01', 'a censored user exits by the forced path', 'fake L2 stand-in: no vault to post requestForced on'); return; }
  const btc = api.market('BTC-USD');
  const maker = ctx.who('maker'), u = ctx.who('censored');
  const forcedOf = async (i, who = u) => (await api.json(`/v1/forced?account=${who.address}`)).forced.find((f) => f.forcedId === String(i));
  const request = async (kind, arg, who = u) => {
    const r = await chain.send(who.address, stack.vault, 'requestForced(uint8,uint64)', [kind, arg]);
    expect(r.ok, `requestForced(${kind}, ${arg}): ${r.error}`);
    const index = Number(await chain.callOne(stack.vault, 'forcedCount()(uint64)')) - 1;
    await chain.waitFinalized(r.block);
    const f = await api.until(async () => { const x = await forcedOf(index, who); return x && x.outcome && x.outcome !== 'pending' ? x : null; }, { ms: 90000, what: `forced request ${index} processed by the engine` });
    return { index, tx: r.tx, l2Block: r.block, forced: f };
  };
  /** The vault learns that the request was processed only from an accepted checkpoint. */
  const acknowledge = async (index, height) => {
    await api.until(async () => {
      const { C, r } = await relayLatest(ctx, { minHeight: height });
      const n = Number((await chain.call(stack.vault, 'latest()(uint64,uint64,uint64,uint64,uint64,uint64,uint128,bytes32,bytes32)'))[3]);
      return n > index ? true : (r.ok, null);
    }, { ms: 90000, every: 2000, what: `a checkpoint with nextForcedIndex > ${index} accepted by the vault` });
  };

  let ref;
  await ctx.check('H-01', 'setup through the API (the last actions this user is "allowed"): a long position and a resting order', async () => {
    ref = (await api.json('/v1/markets/BTC-USD/ticker')).indexPrice;
    await api.cancelAll(maker);
    let r = await api.order(maker, btc, { buy: false, price: onTick(btc, ref, 10), size: '1', tif: 2 });
    expect(r.body?.result?.ok, JSON.stringify(r.body));
    r = await api.order(maker, btc, { buy: true, price: onTick(btc, ref, -10), size: '1', tif: 2 });
    expect(r.body?.result?.ok, JSON.stringify(r.body));
    r = await api.order(u, btc, { buy: true, price: onTick(btc, ref, 50), size: '0.05', type: 1, tif: 1 });
    expect(r.body?.result?.ok, `open: ${JSON.stringify(r.body)}`);
    r = await api.order(u, btc, { buy: true, price: onTick(btc, ref, -300), size: '0.01' });
    expect(r.body?.result?.ok, `rest: ${JSON.stringify(r.body)}`);
    const a = await api.account(u.address);
    expect(a.positions.length === 1 && a.openOrders === 1, JSON.stringify(a).slice(0, 300));
    ctx.state.censoredNonceBefore = (await api.json(`/v1/accounts/${u.address}/nonce`)).nextNonce;
    return { position: a.positions[0].size, openOrders: a.openOrders };
  });
  await ctx.check('H-02', 'forced CancelAll posted on the L2: the engine cancels the order without any signed action', async () => {
    const q = await request(0, 0);
    expect(q.forced.outcome === 'served', JSON.stringify(q.forced));
    const a = await api.account(u.address);
    expect(a.openOrders === 0 && a.positions.length === 1, `orders ${a.openOrders}, positions ${a.positions.length}`);
    await acknowledge(q.index, q.forced.processedHeight);
    return q;
  });
  await ctx.check('H-03', 'forced CloseAll: the position is closed through the book inside the slippage bound', async () => {
    const q = await request(1, 0);
    expect(q.forced.outcome === 'served', JSON.stringify(q.forced));
    const a = await api.account(u.address);
    expect(a.positions.length === 0, `position still open: ${JSON.stringify(a.positions)}`);
    const f = (await api.json(`/v1/accounts/${u.address}/fills?limit=1`)).fills[0];
    const bound = Number(ref) * 0.95;
    expect(f && f.side === 'sell' && Number(f.price) >= bound, `closing fill ${JSON.stringify(f)}`);
    await acknowledge(q.index, q.forced.processedHeight);
    return { ...q, closingFill: f };
  });
  await ctx.check('H-04', 'forced Withdraw of everything: the engine creates the record, the vault pays the user in full', async () => {
    const before = await bal(api, u.address);
    const q = await request(2, 0);
    expect(q.forced.outcome === 'served', JSON.stringify(q.forced));
    expect((await bal(api, u.address)) === 0n, `balance after a withdraw-all: ${fmtUsdc(await bal(api, u.address))}`);
    const w = (await api.json(`/v1/accounts/${u.address}/withdrawals`)).withdrawals[0];
    expect(w && usdc(w.amount) + usdc(w.fee) === before && w.recipient === u.address, `record ${JSON.stringify(w)?.slice(0, 300)} for balance ${fmtUsdc(before)}`);
    const pr = await provenWithdrawal(ctx, w.withdrawalId, q.forced.processedHeight);
    const t0 = BigInt(await chain.callOne(stack.token, 'balanceOf(address)(uint256)', [u.address]));
    const tx = await chain.send(user('relayer').address, stack.vault, W_SIG, [pr.seq, recordTuple(pr.proof.record), `[${pr.proof.siblings.join(',')}]`, pr.proof.record.amount]);
    expect(tx.ok, `withdraw: ${tx.error}`);
    const t1 = BigInt(await chain.callOne(stack.token, 'balanceOf(address)(uint256)', [u.address]));
    expect(t1 - t0 === usdc(w.amount), `paid ${fmtUsdc(t1 - t0)}, record ${w.amount}`);
    return { ...q, engineBalanceBefore: fmtUsdc(before), paidOnL2: fmtUsdc(t1 - t0), fee: w.fee };
  });
  await ctx.check('H-05', 'the user signed nothing after the setup; the vault holds no overdue request and is not in escape', async () => {
    const n = (await api.json(`/v1/accounts/${u.address}/nonce`)).nextNonce;
    expect(n === ctx.state.censoredNonceBefore, `nonce moved: ${ctx.state.censoredNonceBefore} -> ${n}`);
    const forcedCount = Number(await chain.callOne(stack.vault, 'forcedCount()(uint64)'));
    const latest = await chain.call(stack.vault, 'latest()(uint64,uint64,uint64,uint64,uint64,uint64,uint128,bytes32,bytes32)');
    expect(Number(latest[3]) === forcedCount, `vault: processed ${latest[3]} of ${forcedCount}`);
    expect(Number(await chain.callOne(stack.vault, 'escapeReason()(uint8)')) === 0 && (await chain.callOne(stack.vault, 'inEscape()(bool)')) === 'false', 'escape is due');
    await api.cancelAll(maker);
    return { forcedRequests: forcedCount, processedInVault: Number(latest[3]) };
  });
  await ctx.check('H-06', 'the node\'s own checkpoint relayer (the keeper) delivered checkpoints to the vault without anyone asking', async () => {
    blockedIf(!stack.relayer, 'the stack runs without the node relayer (up.sh --relayer off): every checkpoint was relayed by the rehearsal');
    const r = (await api.json('/v1/node')).relayer;
    expect(r.enabled === true && r.accepted >= 1, JSON.stringify(r));
    expect(r.address === stack.relayer, `relayer ${r.address}`);
    return { address: r.address, submitted: r.submitted, accepted: r.accepted, failures: r.failures, lastReason: r.lastReason, vaultLatestHeight: r.vaultLatestHeight, vaultMode: r.vaultMode, alarm: r.alarm };
  });
  // ---- exit from a HALTED market (the oracle goes dark): forced close at the exit band, never at the stale index ----
  const bob = ctx.who('bob');
  let I0, entry;
  await ctx.check('H-07', 'the oracle goes silent: the market halts (STALE); a user\'s forced CloseAll posted on the L2 is still SERVED, at the exit band (at least exitHaircutBps worse than the last index for the closer), against the opposite position', async () => {
    blockedIf(stack.oracle !== 'replay', 'the dev oracle cannot go silent');
    const t = await api.json('/v1/tickers');
    I0 = t.tickers.find((x) => x.symbol === 'BTC-USD').indexPrice;
    const others = Object.fromEntries(t.tickers.filter((x) => x.symbol !== 'BTC-USD').map((x) => [String(api.market(x.symbol).marketId), x.indexPrice]));
    await api.cancelAll(maker);
    entry = onTick(btc, I0, 10);
    let r = await api.order(maker, btc, { buy: false, price: entry, size: '1', tif: 2 });
    expect(r.body?.result?.ok, JSON.stringify(r.body));
    r = await api.order(bob, btc, { buy: true, price: entry, size: '0.05', tif: 1 });
    expect(r.body?.result?.ok && r.body.result.orderStatus === 'filled', `bob open: ${JSON.stringify(r.body)}`);
    await api.cancelAll(maker);
    // 50 rounds (150 s) without any oracle submission, then the same prices again
    await ctx.feed('dark', [{ p: { 1: I0, ...others } }, { silent: 50 }, { p: { 1: I0, ...others }, repeat: 100000 }]);
    const halted = await api.until(async () => { const m = await api.json('/v1/markets/BTC-USD'); return m.status === 'halted' ? m : null; }, { ms: 60000, every: 1000, what: 'BTC-USD halted' });
    const refused = await api.order(bob, btc, { buy: false, price: onTick(btc, I0, -100), size: '0.05', tif: 1, reduceOnly: true }, { wait: false });
    const q = await request(1, 0, bob);
    expect(q.forced.outcome === 'served', JSON.stringify(q.forced));
    const a = await api.account(bob.address);
    expect(!a.positions.some((p) => p.symbol === 'BTC-USD'), `bob still has the position: ${JSON.stringify(a.positions)}`);
    const f = (await api.json(`/v1/accounts/${bob.address}/fills?limit=1`)).fills[0];
    const band = (await api.json('/v1/markets/BTC-USD')).status;
    const hi = Number(I0) * 0.95, lo = Number(I0) * 0.88;
    expect(f.side === 'sell' && Number(f.size) === 0.05 && Number(f.price) <= hi + 1e-6 && Number(f.price) >= lo, `closing fill ${JSON.stringify(f)}: expected a price in [${lo.toFixed(1)}, ${hi.toFixed(1)}] (index ${I0} less the 5% haircut growing 1% per minute)`);
    const mp = (await api.account(maker.address)).positions.find((p) => p.symbol === 'BTC-USD');
    ctx.state.haltedForced = q;
    return { lastIndex: I0, entry, haltReason: halted.haltReason ?? null, apiOrderWhileHalted: refused.body?.error?.code ?? refused.status, marketStatusAtClose: band, forced: q.forced, closingFill: f, haircutPct: ((1 - Number(f.price) / Number(I0)) * 100).toFixed(3), makerPositionAfter: mp?.size ?? '0' };
  });
  await ctx.check('H-08', 'the oracle returns: the market is active again; the vault acknowledges the request in a checkpoint and is not in escape', async () => {
    blockedIf(!ctx.state.haltedForced, 'H-07 did not run');
    const m = await api.until(async () => { const x = await api.json('/v1/markets/BTC-USD'); return x.status === 'active' ? x : null; }, { ms: 300000, every: 3000, what: 'BTC-USD active again' });
    await acknowledge(ctx.state.haltedForced.index, ctx.state.haltedForced.forced.processedHeight);
    expect(Number(await chain.callOne(stack.vault, 'escapeReason()(uint8)')) === 0, 'escape is due');
    const mp = (await api.account(maker.address)).positions.find((p) => p.symbol === 'BTC-USD');
    return { status: m.status, index: (await api.json('/v1/markets/BTC-USD/ticker')).indexPrice, makerPosition: mp?.size ?? '0' };
  });
}
