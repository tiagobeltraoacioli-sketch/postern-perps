// Phase S: the daily fee sweep. Fees accrue in the engine's fee account; when ENGINE time crosses a UTC day boundary
// the whole balance becomes a withdrawal record to the fee recipient fixed in the vault, which the vault pays.
// Needs a stack started with the node's dev clock (up.sh --clock-offset MS: debug build, dev oracle): the offset is a
// start-time value, so the day is crossed by restarting every process with a larger one. UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { usdc, sleep } from '../../lib/client.mjs';
import { bal, fmtUsdc, tradeFee, onTick, SYSTEM, OWNER_FEE_ADDRESS } from '../util.mjs';
import { user } from '../../lib/keys.mjs';
import { W_SIG, recordTuple, provenWithdrawal } from './G-withdraw.mjs';

export default async function (ctx) {
  const { api, chain, stack } = ctx;
  const on = stack.clockOffsetMs !== null && stack.clockOffsetMs !== undefined;
  if (!on) { await ctx.blocked('S-01', 'the daily fee sweep pays the fee recipient through the vault', 'the stack runs without the dev clock (up.sh --clock-offset MS); the engine time cannot cross a UTC day in a rehearsal otherwise'); return; }
  const btc = api.market('BTC-USD');
  const maker = ctx.who('maker'), alice = ctx.who('alice');
  const DAY = 86400000n;
  let fees, day0, swept;
  await ctx.check('S-01', 'fees accrue: a 2 BTC round trip between two accounts leaves exactly its four fees in the fee account, above the sweep minimum (100 USDC)', async () => {
    const f0 = await bal(api, SYSTEM.fee);
    const ref = (await api.json('/v1/markets/BTC-USD/ticker')).indexPrice;
    const ask = onTick(btc, ref, 10), bid = onTick(btc, ref, -10);
    await api.cancelAll(maker);
    let r = await api.order(maker, btc, { buy: false, price: ask, size: '2', tif: 2 }); expect(r.body?.result?.ok, JSON.stringify(r.body));
    r = await api.order(maker, btc, { buy: true, price: bid, size: '2', tif: 2 }); expect(r.body?.result?.ok, JSON.stringify(r.body));
    r = await api.order(alice, btc, { buy: true, price: ask, size: '2', tif: 1 }); expect(r.body?.result?.orderStatus === 'filled', `open: ${JSON.stringify(r.body)}`);
    r = await api.order(alice, btc, { buy: false, price: bid, size: '2', tif: 1, reduceOnly: true }); expect(r.body?.result?.orderStatus === 'filled', `close: ${JSON.stringify(r.body)}`);
    await api.cancelAll(maker);
    const want = tradeFee(ask, '2', 450) + tradeFee(ask, '2', 150) + tradeFee(bid, '2', 450) + tradeFee(bid, '2', 150);
    fees = (await bal(api, SYSTEM.fee));
    expect(fees - f0 === want, `fee account +${fmtUsdc(fees - f0)}, the four fees are ${fmtUsdc(want)}`);
    expect(fees >= usdc('100'), `fee account ${fmtUsdc(fees)} is below the sweep minimum`);
    day0 = BigInt((await api.json('/v1/status')).time) / DAY;
    return { feeAccount: fmtUsdc(fees), fromThisRoundTrip: fmtUsdc(want), engineDay: String(day0) };
  });
  await ctx.check('S-02', 'the engine clock crosses 00:00 UTC (every process restarted with the same larger dev offset): the fee account is swept to zero into ONE withdrawal record for the fee recipient, for exactly its balance', async () => {
    expect(fees !== undefined, 'S-01 failed');
    const now = BigInt((await api.json('/v1/status')).time);
    const before = (await api.json(`/v1/accounts/${SYSTEM.fee}/withdrawals`)).withdrawals.length;
    const newOffset = BigInt(stack.clockOffsetMs) + (DAY - (now % DAY)) + 120000n;
    await ctx.host('clock-advance.sh', String(newOffset));
    await api.until(async () => BigInt((await api.json('/v1/status')).time) / DAY > day0, { ms: 90000, every: 2000, what: 'the engine in the next UTC day' });
    const w = await api.until(async () => { await api.cancelAll(alice); const l = (await api.json(`/v1/accounts/${SYSTEM.fee}/withdrawals`)).withdrawals; return l.length > before ? l[0] : null; }, { ms: 90000, every: 2000, what: 'the fee sweep record' });
    expect((await bal(api, SYSTEM.fee)) < usdc('100'), `fee account after the sweep: ${fmtUsdc(await bal(api, SYSTEM.fee))}`);
    expect(w.recipient === OWNER_FEE_ADDRESS && w.account === SYSTEM.fee, `record ${JSON.stringify(w).slice(0, 300)}`);
    expect(usdc(w.amount) + usdc(w.fee) === fees && (await bal(api, SYSTEM.fee)) === usdc(w.fee), `record ${w.amount} + fee ${w.fee} against the balance ${fmtUsdc(fees)}; left in the account ${fmtUsdc(await bal(api, SYSTEM.fee))}`);
    swept = w;
    return { newOffsetMs: String(newOffset), engineDayAfter: String(BigInt((await api.json('/v1/status')).time) / DAY), record: { withdrawalId: w.withdrawalId, amount: w.amount, fee: w.fee, recipient: w.recipient }, feeAccountAfter: fmtUsdc(await bal(api, SYSTEM.fee)) };
  });
  await ctx.check('S-03', 'the vault pays that record to the owner\'s fee address and to nobody else, exactly once', async () => {
    expect(swept, 'S-02 failed');
    const tokenBal = async (a) => BigInt(await chain.callOne(stack.token, 'balanceOf(address)(uint256)', [a]));
    const pr = await provenWithdrawal(ctx, swept.withdrawalId, swept.requestedHeight);
    const rec = pr.proof.record, from = user('relayer').address;
    const other = await chain.send(from, stack.vault, W_SIG, [pr.seq, recordTuple({ ...rec, recipient: from }), `[${pr.proof.siblings.join(',')}]`, rec.amount]);
    expect(!other.ok, 'the fee record was paid to another recipient');
    const o0 = await tokenBal(OWNER_FEE_ADDRESS);
    const tx = await chain.send(from, stack.vault, W_SIG, [pr.seq, recordTuple(rec), `[${pr.proof.siblings.join(',')}]`, rec.amount]);
    expect(tx.ok, `withdraw: ${tx.error}`);
    const got = (await tokenBal(OWNER_FEE_ADDRESS)) - o0;
    expect(got === usdc(swept.amount), `the fee address received ${fmtUsdc(got)}, the record is ${swept.amount}`);
    const again = await chain.send(from, stack.vault, W_SIG, [pr.seq, recordTuple(rec), `[${pr.proof.siblings.join(',')}]`, rec.amount]);
    expect(!again.ok && (await tokenBal(OWNER_FEE_ADDRESS)) - o0 === got, 'the record was paid twice');
    return { paidToFeeRecipient: fmtUsdc(got), feeRecipient: OWNER_FEE_ADDRESS, tx: tx.tx, otherRecipient: other.error, second: again.error };
  });
  await ctx.check('S-04', 'the default build has no dev clock: the release binary refuses --dev-clock-offset-ms at start', async () => {
    const out = await ctx.host('clock-refused.sh', stack.label);
    expect(/REFUSED/.test(out), out.slice(0, 300));
    return { releaseBinary: out.slice(0, 300) };
  });
}
