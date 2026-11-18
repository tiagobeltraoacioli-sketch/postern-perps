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
  const forcedOf = async (i) => (await api.json(`/v1/forced?account=${u.address}`)).forced.find((f) => f.forcedId === String(i));
  const request = async (kind, arg) => {
    const r = await chain.send(u.address, stack.vault, 'requestForced(uint8,uint64)', [kind, arg]);
    expect(r.ok, `requestForced(${kind}, ${arg}): ${r.error}`);
    const index = Number(await chain.callOne(stack.vault, 'forcedCount()(uint64)')) - 1;
    await chain.waitFinalized(r.block);
    const f = await api.until(async () => { const x = await forcedOf(index); return x && x.outcome && x.outcome !== 'pending' ? x : null; }, { ms: 90000, what: `forced request ${index} processed by the engine` });
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
}
