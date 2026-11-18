// Phase C: orders, fills, exact fees to the fee account, refusals, conservation. UNAUDITED prototype.
import { expect } from '../check.mjs';
import { usdc, px, sz, sleep } from '../../lib/client.mjs';
import { bal, fmtUsdc, tradeFee, onTick, totalBalances, SYSTEM } from '../util.mjs';
import { user } from '../../lib/keys.mjs';

export default async function (ctx) {
  const { api, CAST } = ctx;
  const btc = api.market('BTC-USD');
  const maker = ctx.who('maker'), alice = ctx.who('alice');
  const t = await api.json('/v1/markets/BTC-USD/ticker');
  // reference: the index when the API shows one, else the dev oracle's fixed BTC price (node/src/dev.rs)
  const ref = Number(t.indexPrice) > 0 ? t.indexPrice : '67250';
  const ask = onTick(btc, ref, 10), bid = onTick(btc, ref, -10);
  const size = '0.2';
  ctx.state.ref = ref;

  await ctx.check('C-01', 'the maker rests a post-only bid and ask; the public book shows both', async () => {
    const a = await api.order(maker, btc, { buy: false, price: ask, size: '1', tif: 2 });
    const b = await api.order(maker, btc, { buy: true, price: bid, size: '1', tif: 2 });
    expect(a.body?.result?.ok && b.body?.result?.ok, `${JSON.stringify(a.body)} ${JSON.stringify(b.body)}`);
    const book = await api.json('/v1/markets/BTC-USD/book?depth=5');
    expect(book.asks.some(([p]) => Number(p) === Number(ask)) && book.bids.some(([p]) => Number(p) === Number(bid)), JSON.stringify(book));
    return { bid, ask, book: { bids: book.bids, asks: book.asks } };
  });

  let before;
  await ctx.check('C-02', 'a taker buy fills at the maker\'s price and pays exactly ceil(notional x 4.5 bps)', async () => {
    before = { alice: await bal(api, alice.address), maker: await bal(api, maker.address), fee: await bal(api, SYSTEM.fee) };
    const r = await api.order(alice, btc, { buy: true, price: onTick(btc, ref, 50), size, type: 1, tif: 1 });
    expect(r.body?.status === 'included' && r.body?.result?.ok === true, JSON.stringify(r.body));
    const fee = tradeFee(ask, size, 450);
    const got = before.alice - (await bal(api, alice.address));
    expect(got === fee, `alice paid ${fmtUsdc(got)}, expected fee ${fmtUsdc(fee)} at ${ask}`);
    return { price: ask, size, takerFee: fmtUsdc(fee), height: r.body.height };
  });
  await ctx.check('C-03', 'the maker pays exactly ceil(notional x 1.5 bps) and the fee account receives exactly taker + maker fee', async () => {
    const tf = tradeFee(ask, size, 450), mf = tradeFee(ask, size, 150);
    const m = before.maker - (await bal(api, maker.address));
    const f = (await bal(api, SYSTEM.fee)) - before.fee;
    expect(m === mf, `maker paid ${fmtUsdc(m)}, expected ${fmtUsdc(mf)}`);
    expect(f === tf + mf, `fee account +${fmtUsdc(f)}, expected ${fmtUsdc(tf + mf)}`);
    return { makerFee: fmtUsdc(mf), feeAccountDelta: fmtUsdc(f) };
  });
  await ctx.check('C-04', 'both positions are published with the entry price, equal and opposite (I-1)', async () => {
    const pa = (await api.account(alice.address)).positions.find((p) => p.symbol === 'BTC-USD');
    const pm = (await api.account(maker.address)).positions.find((p) => p.symbol === 'BTC-USD');
    expect(pa && pm, 'the API publishes no position for the two sides of the fill');
    expect(Number(pa.size) === Number(size) && Number(pm.size) === -Number(size), `${pa.size} / ${pm.size}`);
    expect(Number(pa.entryPrice) === Number(ask), `entry ${pa.entryPrice}`);
    return { alice: pa, maker: pm };
  });
  await ctx.check('C-05', 'the fill is listed for taker and maker with its fee and liquidity side, and in the public trades', async () => {
    const fa = (await api.json(`/v1/accounts/${alice.address}/fills?limit=5`)).fills[0];
    const fm = (await api.json(`/v1/accounts/${maker.address}/fills?limit=5`)).fills[0];
    expect(fa && fm, 'the API lists no fill');
    expect(fa.liquidity === 'taker' && fm.liquidity === 'maker' && fa.tradeId === fm.tradeId, `${JSON.stringify(fa)} ${JSON.stringify(fm)}`);
    expect(usdc(fa.fee) === tradeFee(ask, size, 450) && usdc(fm.fee) === tradeFee(ask, size, 150), `fees ${fa.fee} / ${fm.fee}`);
    const tr = (await api.json('/v1/markets/BTC-USD/trades?limit=5')).trades[0];
    expect(tr && tr.tradeId === fa.tradeId && tr.side === 'buy' && Number(tr.price) === Number(ask), JSON.stringify(tr));
    return { taker: fa, maker: fm, trade: tr };
  });
  await ctx.check('C-06', 'the engine refuses: post-only that would cross, an order beyond the margin, price 0, an account that never deposited', async () => {
    const out = {};
    let r = await api.order(maker, btc, { buy: true, price: onTick(btc, ref, 30), size: '0.01', tif: 2 });
    out.postOnlyCross = r.body?.result?.error;
    expect(r.body?.result?.ok === false && out.postOnlyCross === 'WOULD_CROSS', JSON.stringify(r.body));
    r = await api.order(alice, btc, { buy: true, price: bid, size: '9', tif: 0 });   // ~605,000 USD notional on 20,000 USDC
    out.overMargin = r.body?.result?.error;
    expect(r.body?.result?.ok === false && out.overMargin === 'INSUFFICIENT_MARGIN', JSON.stringify(r.body));
    r = await api.act(alice, 'PlaceOrder', { marketId: btc.marketId, isBuy: true, orderType: 1, timeInForce: 1, price: '0', size: '2000000', reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) });
    out.priceZero = r.body?.error?.code ?? r.body?.result?.error;
    expect(out.priceZero === 'MALFORMED', JSON.stringify(r.body));
    const ghost = user('never-deposited');
    r = await api.order(ghost, btc, { buy: true, price: bid, size: '0.01' });
    out.neverDeposited = r.body?.error?.code ?? r.body?.result?.error;
    expect(r.body?.result?.ok !== true && ['UNKNOWN_ACCOUNT', 'INSUFFICIENT_MARGIN'].includes(out.neverDeposited), JSON.stringify(r.body));
    return out;
  });
  await ctx.check('C-07', 'an agent wallet approved by the main key trades for the account and can never withdraw', async () => {
    const agent = user(`agent-of-alice/${Date.now()}`);
    const now = BigInt((await api.json('/v1/status')).time);
    let r = await api.act(alice, 'ApproveAgent', { agent: agent.address, name: 'rehearsal', expiresAt: String(now + 3600000n) });
    expect(r.body?.result?.ok === true, `approve: ${JSON.stringify(r.body)}`);
    r = await api.order(agent, btc, { buy: true, price: onTick(btc, ref, -100), size: '0.01' }, { account: alice.address });
    expect(r.body?.result?.ok === true, `agent order: ${JSON.stringify(r.body)}`);
    const w = await api.act(agent, 'Withdraw', { amount: '5000000', recipient: agent.address }, { account: alice.address });
    expect(w.status === 403 && w.body?.error?.code === 'AGENT_NOT_ALLOWED', `agent withdraw: ${w.status} ${JSON.stringify(w.body)}`);
    r = await api.act(alice, 'RevokeAgent', { agent: agent.address });
    expect(r.body?.result?.ok === true, `revoke: ${JSON.stringify(r.body)}`);
    const after = await api.order(agent, btc, { buy: true, price: onTick(btc, ref, -100), size: '0.01' }, { account: alice.address });
    expect(after.status === 403, `order by the revoked agent: ${after.status} ${JSON.stringify(after.body)}`);
    await api.cancelAll(alice);
    return { agent: agent.address, agentWithdraw: 'AGENT_NOT_ALLOWED', afterRevoke: after.body?.error?.code };
  });
  await ctx.check('C-08', 'alice closes with a reduce-only order; with every position flat the balances of all accounts sum exactly to the deposits (I-2)', async () => {
    const r = await api.order(alice, btc, { buy: false, price: onTick(btc, ref, -50), size, type: 1, tif: 1, reduceOnly: true });
    expect(r.body?.result?.ok === true, JSON.stringify(r.body));
    await api.cancelAll(maker);
    await sleep(500);
    const deposits = CAST.reduce((s, a) => s + usdc(a.deposit), 0n);
    const tb = await totalBalances(api, CAST);
    expect(tb.total === deposits, `sum of balances ${fmtUsdc(tb.total)} vs deposits ${fmtUsdc(deposits)} (difference ${fmtUsdc(tb.total - deposits)})`);
    // the round trip: alice lost the spread and two taker fees, nothing else
    const spread = ((px(ask) - px(bid)) * sz(size)) / 10n ** 10n;   // exact: both prices on the tick, round size
    const aliceLoss = usdc(alice.deposit) - (await bal(api, alice.address));
    const expectLoss = spread + tradeFee(ask, size, 450) + tradeFee(bid, size, 450);
    expect(aliceLoss === expectLoss, `alice lost ${fmtUsdc(aliceLoss)}, expected spread + fees ${fmtUsdc(expectLoss)}`);
    return { deposits: fmtUsdc(deposits), sumOfBalances: fmtUsdc(tb.total), balances: tb.per, aliceRoundTripCost: fmtUsdc(aliceLoss) };
  });
}
