// Phase G: a withdrawal signed by the user, certified by the committee, proven and paid by the vault.
// UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { usdc, sleep } from '../../lib/client.mjs';
import { bal, fmtUsdc, SYSTEM } from '../util.mjs';
import { user } from '../../lib/keys.mjs';
import { relayLatest } from './F-finality.mjs';

export const W_SIG = 'withdraw(uint64,(uint64,address,address,uint64,uint64),bytes32[],uint64)';
export const recordTuple = (r) => `(${r.withdrawalId},${r.account},${r.recipient},${r.amount},${r.height})`;

/** Relays a checkpoint that contains withdrawal `id`, fetches its proof for that checkpoint. */
export async function provenWithdrawal(ctx, id, minHeight) {
  const { api, chain, stack } = ctx;
  const { C, r, held } = await relayLatest(ctx, { minHeight });
  const seq = Number(await chain.callOne(stack.vault, 'checkpointCount()(uint64)'));
  expect(held, `checkpoint relay: ${r.error}`);
  const count = Number(await chain.callOne(stack.vault, 'withdrawalCountAt(uint64)(uint64)', [seq]));
  expect(count > Number(id), `the accepted checkpoint ${seq} holds ${count} withdrawal records, need id ${id}`);
  const p = await api.json(`/v1/withdrawals/${id}/proof?count=${count}`);
  return { seq, count, proof: p, checkpoint: C };
}

export default async function (ctx) {
  const { api, chain, stack } = ctx;
  const alice = ctx.who('alice'), maker = ctx.who('maker');
  const payee = user(`payee/${Date.now()}`).address;   // a fresh L2 address: its token balance starts at 0
  const tokenBal = async (a) => BigInt(await chain.callOne(stack.token, 'balanceOf(address)(uint256)', [a]));
  let W, proven;

  await ctx.check('G-01', 'a signed Withdraw debits the gross amount, credits the flat fee to the fee account and creates a record for the net amount', async () => {
    const [a0, f0] = [await bal(api, alice.address), await bal(api, SYSTEM.fee)];
    const r = await api.withdraw(alice, '1000', payee);
    expect(r.body?.result?.ok === true && r.body.result.withdrawalId !== undefined, JSON.stringify(r.body));
    const [a1, f1] = [await bal(api, alice.address), await bal(api, SYSTEM.fee)];
    expect(a0 - a1 === usdc('1000'), `alice debited ${fmtUsdc(a0 - a1)}`);
    W = await api.json(`/v1/withdrawals/${r.body.result.withdrawalId}`);
    expect(f1 - f0 === usdc(W.fee) && usdc(W.fee) > 0n, `fee account +${fmtUsdc(f1 - f0)}, record fee ${W.fee}`);
    expect(usdc(W.amount) === usdc('1000') - usdc(W.fee) && W.recipient === payee && W.account === alice.address, JSON.stringify(W));
    return { withdrawalId: W.withdrawalId, gross: '1000.000000', fee: W.fee, net: W.amount, recipient: payee, height: r.body.height };
  });
  await ctx.check('G-02', 'the record becomes certified: a final checkpoint with 5 committee signatures commits it', async () => {
    const w = await api.until(async () => { const x = await api.json(`/v1/withdrawals/${W.withdrawalId}`); return x.status === 'certified' ? x : null; }, { ms: 60000, what: 'certified' });
    expect(w.certificate && w.certificate.signatures.length >= 5 && w.certificate.checkpoint.withdrawalCount > Number(W.withdrawalId), JSON.stringify(w).slice(0, 300));
    return { status: w.status, checkpointHeight: w.checkpointHeight, withdrawalCount: w.certificate.checkpoint.withdrawalCount };
  });
  await ctx.check('G-03', 'anyone submits the checkpoint and the depth-32 proof: the vault pays the recipient exactly the net amount, once', async () => {
    blockedIf(!chain, 'fake L2 stand-in: no vault');
    proven = await provenWithdrawal(ctx, W.withdrawalId, W.requestedHeight);
    const rec = proven.proof.record;
    expect(proven.proof.siblings.length === 32, `${proven.proof.siblings.length} siblings`);
    expect(BigInt(rec.amount) === usdc(W.amount) && rec.recipient === payee, `record ${JSON.stringify(rec)}`);
    const [p0, c0] = [await tokenBal(payee), BigInt(await chain.callOne(stack.vault, 'custody()(uint64)'))];
    const stranger = user('relayer').address;
    const tx = await chain.send(stranger, stack.vault, W_SIG, [proven.seq, recordTuple(rec), `[${proven.proof.siblings.join(',')}]`, rec.amount]);
    expect(tx.ok, `withdraw: ${tx.error}`);
    const [p1, c1] = [await tokenBal(payee), BigInt(await chain.callOne(stack.vault, 'custody()(uint64)'))];
    expect(p1 - p0 === usdc(W.amount), `recipient received ${fmtUsdc(p1 - p0)}, record ${W.amount}`);
    expect(c0 - c1 === usdc(W.amount), `custody fell by ${fmtUsdc(c0 - c1)}`);
    expect((await tokenBal(stranger)) === 0n, 'the submitter received tokens');
    return { seq: proven.seq, tx: tx.tx, gas: tx.gas, paid: fmtUsdc(p1 - p0), custodyAfter: fmtUsdc(c1) };
  });
  await ctx.check('G-04', 'the vault refuses: the same record again, the record with another amount, another recipient, or a wrong sibling', async () => {
    blockedIf(!chain || !proven, 'no proven withdrawal to attack');
    const rec = proven.proof.record, sib = proven.proof.siblings, from = user('relayer').address;
    const tryIt = (r, s = sib) => chain.send(from, stack.vault, W_SIG, [proven.seq, recordTuple(r), `[${s.join(',')}]`, r.amount]);
    const again = await tryIt(rec);
    const more = await tryIt({ ...rec, amount: String(BigInt(rec.amount) + 1n) });
    const other = await tryIt({ ...rec, recipient: from });
    const bad = await tryIt(rec, [sib[1], sib[0], ...sib.slice(2)]);
    const p0 = await tokenBal(payee);
    expect(!again.ok && !more.ok && !other.ok && !bad.ok, `accepted: again ${again.ok}, amount ${more.ok}, recipient ${other.ok}, sibling ${bad.ok}`);
    expect(p0 === usdc(W.amount) && (await tokenBal(from)) === 0n, 'a refused attempt moved tokens');
    return { again: again.error, amount: more.error, recipient: other.error, sibling: bad.error };
  });
  await ctx.check('G-05', 'a withdrawal at or above the large threshold is registered but paid only after the large-amount delay (L2 clock moved one day)', async () => {
    blockedIf(!chain, 'fake L2 stand-in: no vault');
    const big = user(`payee-large/${Date.now()}`).address;
    const large = BigInt(await chain.callOne(stack.vault, 'largeThreshold()(uint64)'));
    const delay = Number(await chain.callOne(stack.vault, 'largeDelay()(uint64)'));
    const fee = usdc(W.fee);
    const gross = large + fee;                                  // net = exactly the threshold
    const r = await api.act(maker, 'Withdraw', { amount: String(gross), recipient: big });
    expect(r.body?.result?.ok === true, JSON.stringify(r.body));
    const id = r.body.result.withdrawalId;
    const pr = await provenWithdrawal(ctx, id, r.body.height);
    const rec = pr.proof.record, from = user('relayer').address;
    const tx = await chain.send(from, stack.vault, W_SIG, [pr.seq, recordTuple(rec), `[${pr.proof.siblings.join(',')}]`, rec.amount]);
    expect(tx.ok, `register: ${tx.error}`);
    expect((await tokenBal(big)) === 0n, 'a large withdrawal was paid at once');
    const key = await chain.callOne(stack.vault, 'withdrawalKey(uint64)(bytes32)', [id]);
    const early = await chain.send(from, stack.vault, 'release(bytes32,uint64)', [key, rec.amount]);
    expect(!early.ok, 'released before the delay');
    await chain.warp(delay + 5);
    const late = await chain.send(from, stack.vault, 'release(bytes32,uint64)', [key, rec.amount]);
    expect(late.ok, `release after the delay: ${late.error}`);
    expect((await tokenBal(big)) === large, `paid ${fmtUsdc(await tokenBal(big))}`);
    return { withdrawalId: id, net: fmtUsdc(large), delaySeconds: delay, releasedEarly: early.error, paidAfterDelay: fmtUsdc(large) };
  });
  // ---- the node's own relayer (the keeper) against a lying or absurd L2 endpoint (R436-15) ----
  const relayer = async () => (await api.json('/v1/node')).relayer;
  const fault = async (m) => { const r = await fetch(`${stack.l2Proxy}/__fault`, { method: 'POST', body: JSON.stringify({ mode: m }) }); expect(r.status === 200, `proxy fault ${m}`); };
  const vaultCount = async () => Number((await chain.call(stack.vault, 'latest()(uint64,uint64,uint64,uint64,uint64,uint64,uint128,bytes32,bytes32)'))[4]);
  for (const [id, mode, what, pattern, title] of [
    ['G-06', 'gas', 'names an absurd gas price', /GAS PRICE ABOVE THE CAP/i, 'the relayer signs nothing (gas cap, loud error) while a checkpoint is due; it delivers when the price is sane again'],
    ['G-07', 'hash', 'reports another block hash', /disagree/i, 'the relayer refuses to act on readings its two endpoints do not agree on; it delivers when they agree again'],
  ]) {
    await ctx.check(id, `one L2 endpoint ${what}: ${title}`, async () => {
      blockedIf(!chain || !stack.relayer, 'the stack runs without the node relayer');
      const r0 = await relayer();
      await fault(mode);
      let during, countDuring, id2;
      try {
        const w = await api.withdraw(alice, '10', payee);            // a new record: the keeper must deliver a checkpoint at once
        expect(w.body?.result?.ok === true, JSON.stringify(w.body));
        id2 = Number(w.body.result.withdrawalId);
        await api.until(async () => (await api.json(`/v1/withdrawals/${id2}`)).status === 'certified', { ms: 60000, what: 'certified' });
        // baseline = the moment the relayer first reports the fault for THIS due checkpoint (a delivery it had begun
        // for an earlier record may still land between the fault being set and its next reading)
        const first = await api.until(async () => { const r = await relayer(); return pattern.test(r.lastError ?? '') || pattern.test(r.alarm ?? '') ? r : null; }, { ms: 60000, every: 1000, what: `the relayer reporting ${pattern}` });
        await sleep(8000);
        during = await relayer();
        countDuring = await vaultCount();
        expect(pattern.test(during.lastError ?? '') || pattern.test(during.alarm ?? ''), `the relayer stopped reporting the fault: ${JSON.stringify(during).slice(0, 200)}`);
        expect(during.submitted === first.submitted, `the relayer submitted ${during.submitted - first.submitted} transaction(s) after it had seen that one endpoint ${what}`);
        expect(countDuring <= id2, `the vault received a checkpoint with ${countDuring} records while the relayer should have held`);
      } finally {
        await fault('none');
      }
      const after = await api.until(async () => { const r = await relayer(); return r.accepted > r0.accepted && (await vaultCount()) > id2 ? r : null; }, { ms: 90000, every: 1500, what: 'delivery after the fault' });
      return { withdrawalId: id2, whileFaulty: { submitted: during.submitted, gasRefused: during.gasRefused ?? null, lastError: (during.lastError ?? '').slice(0, 200), alarm: during.alarm }, vaultRecordsWhileFaulty: countDuring, afterwards: { submitted: after.submitted, accepted: after.accepted, vaultRecords: await vaultCount() } };
    });
  }
}
