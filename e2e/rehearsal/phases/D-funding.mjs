// Phase D: funding over several intervals, exact to the unit, including the capped rate. UNAUDITED prototype.
// The stack's dev genesis shortens the funding interval (up.sh --funding-ms, default one minute; production 1 h).
import { expect, blockedIf } from '../check.mjs';
import { usdc, sleep } from '../../lib/client.mjs';
import { bal, fmtUsdc, onTick, fundingPayment, totalBalances, creditedDeposits, withdrawalRecords } from '../util.mjs';

export default async function (ctx) {
  const { api, CAST } = ctx;
  const btc = await api.json('/v1/markets/BTC-USD');
  const maker = ctx.who('maker'), bob = ctx.who('bob');
  const iv = btc.fundingIntervalSec * 1000;
  const short = iv <= 120000;
  const index = async () => (await api.json('/v1/markets/BTC-USD/ticker')).indexPrice;
  const size = '0.5';
  const payments = async (a, since) => (await api.json(`/v1/accounts/${a}/funding?symbol=BTC-USD&limit=50`)).payments.filter((p) => p.time > since);
  const boundaries = async (n, since) => api.until(async () => { const f = (await api.json('/v1/markets/BTC-USD/funding?limit=50')).funding.filter((x) => x.time > since); return f.length >= n ? f : null; }, { ms: (n + 1) * iv + 30000, every: 2000, what: `${n} funding settlements` });

  await ctx.check('D-01', 'the market publishes its funding interval and cap; this stack runs the stated dev override', async () => {
    blockedIf(!short, `funding interval is ${btc.fundingIntervalSec} s: several intervals do not fit a rehearsal (start the stack with --funding-ms 60000)`);
    expect(Number(btc.maxFundingRate) === 0.0005, `maxFundingRate ${btc.maxFundingRate}`);
    return { fundingIntervalSec: btc.fundingIntervalSec, maxFundingRate: btc.maxFundingRate, production: '3600 s (compliance/markets.json)' };
  });
  if (!short) {
    await ctx.blocked('D-02', 'funding is paid over three intervals, each payment exact to the unit', 'funding interval too long for a rehearsal');
    return;
  }

  let t0, b0, m0;
  await ctx.check('D-02', 'three funding intervals at the interest rate: every payment equals size x mark x rate exactly (payer rounded up, receiver down) and the balances moved by exactly their sum', async () => {
    const ref = await index();
    let r = await api.order(maker, btc, { buy: false, price: onTick(btc, ref, 10), size: '1', tif: 2 });
    expect(r.body?.result?.ok, `maker ask: ${JSON.stringify(r.body)}`);
    r = await api.order(maker, btc, { buy: true, price: onTick(btc, ref, -10), size: '1', tif: 2 });
    expect(r.body?.result?.ok, `maker bid: ${JSON.stringify(r.body)}`);
    r = await api.order(bob, btc, { buy: true, price: onTick(btc, ref, 50), size, type: 1, tif: 1 });
    expect(r.body?.result?.ok, `bob buy: ${JSON.stringify(r.body)}`);
    t0 = (await api.json('/v1/status')).time;
    b0 = await bal(api, bob.address); m0 = await bal(api, maker.address);
    const rates = await boundaries(3, t0);
    await sleep(1500);
    const pb = await payments(bob.address, t0), pm = await payments(maker.address, t0);
    expect(pb.length >= 3 && pm.length === pb.length, `payments: bob ${pb.length}, maker ${pm.length}`);
    let sb = 0n, sm = 0n;
    for (const p of pb) {
      const want = fundingPayment(p.positionSize, p.markPrice, p.rate);
      expect(usdc(p.payment) === want, `bob at ${p.time}: paid ${p.payment}, exact ${fmtUsdc(want)} (size ${p.positionSize}, mark ${p.markPrice}, rate ${p.rate})`);
      expect(Number(p.rate) > 0 && usdc(p.payment) < 0n, 'a long must pay a positive rate');
      sb += usdc(p.payment);
    }
    for (const p of pm) {
      const want = fundingPayment(p.positionSize, p.markPrice, p.rate);
      expect(usdc(p.payment) === want, `maker at ${p.time}: got ${p.payment}, exact ${fmtUsdc(want)}`);
      sm += usdc(p.payment);
    }
    const b1 = await bal(api, bob.address), m1 = await bal(api, maker.address);
    // re-read: a settlement between the two reads would show as a difference
    const pb2 = await payments(bob.address, t0);
    if (pb2.length === pb.length) {
      expect(b1 - b0 === sb, `bob's balance moved ${fmtUsdc(b1 - b0)}, his payments sum to ${fmtUsdc(sb)}`);
      expect(m1 - m0 === sm, `maker's balance moved ${fmtUsdc(m1 - m0)}, its payments sum to ${fmtUsdc(sm)}`);
    }
    expect(sb + sm <= 0n && sb + sm >= -BigInt(pb.length), `payer and receiver do not net to the rounding residue: ${fmtUsdc(sb + sm)}`);
    expect(rates.every((x) => Math.abs(Number(x.rate)) <= 0.0005), 'a rate above the cap');
    return { intervals: pb.length, bob: pb.map((p) => ({ time: p.time, rate: p.rate, mark: p.markPrice, payment: p.payment })), bobTotal: fmtUsdc(sb), makerTotal: fmtUsdc(sm), residueToInsurance: fmtUsdc(-(sb + sm)) };
  });

  await ctx.check('D-03', 'a book quoted above the index raises the premium: the rate settles at the cap (+0.05% per interval) and never above it', async () => {
    const ref = await index();
    await api.cancelAll(maker);
    let r = await api.order(maker, btc, { buy: true, price: onTick(btc, ref, 20), size: '1', tif: 2 });
    expect(r.body?.result?.ok, `bid above index: ${JSON.stringify(r.body)}`);
    r = await api.order(maker, btc, { buy: false, price: onTick(btc, ref, 40), size: '1', tif: 2 });
    expect(r.body?.result?.ok, `ask above index: ${JSON.stringify(r.body)}`);
    const t1 = (await api.json('/v1/status')).time;
    // the rate is the MEDIAN premium of the interval (R395-16): the first settlement may still hold older samples
    const f = await boundaries(2, t1);
    const last = f[0];
    expect(last.rate === '0.0005', `rate after a full interval of premium: ${last.rate} (premium ${last.premium})`);
    const p = (await payments(bob.address, last.time - 1)).find((x) => x.time === last.time);
    expect(p && usdc(p.payment) === fundingPayment(p.positionSize, p.markPrice, p.rate), `bob's capped payment: ${JSON.stringify(p)}`);
    return { settlement: last, bobPayment: p };
  });

  await ctx.check('D-04', 'positions closed, quotes cancelled: the balances of all accounts again sum exactly to deposits minus withdrawals (funding created or destroyed nothing)', async () => {
    const ref = await index();
    await api.cancelAll(maker);
    let r = await api.order(maker, btc, { buy: true, price: onTick(btc, ref, -10), size: '1', tif: 2 });
    expect(r.body?.result?.ok, JSON.stringify(r.body));
    r = await api.order(bob, btc, { buy: false, price: onTick(btc, ref, -60), size, type: 1, tif: 1, reduceOnly: true });
    expect(r.body?.result?.ok, `bob close: ${JSON.stringify(r.body)}`);
    await api.cancelAll(maker);
    await sleep(600);
    for (const a of [bob, maker]) expect((await api.account(a.address)).positions.length === 0, `${a.label} still has a position`);
    const net = (await creditedDeposits(api, CAST)) - (await withdrawalRecords(api, CAST));
    const tb = await totalBalances(api, CAST);
    expect(tb.total === net, `sum of balances ${fmtUsdc(tb.total)} vs deposits - withdrawals ${fmtUsdc(net)} (difference ${fmtUsdc(tb.total - net)})`);
    return { depositsMinusWithdrawals: fmtUsdc(net), sumOfBalances: fmtUsdc(tb.total), insurance: tb.per.insurance };
  });
}
