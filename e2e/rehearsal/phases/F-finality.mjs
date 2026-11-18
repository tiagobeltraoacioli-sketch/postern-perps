// Phase F: checkpoints signed 5 of 7, recomputed independently, replayed by the engine CLI, accepted by the vault.
// UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { unhex } from '../../../node/mock/lib/keccak.mjs';
import { recover } from '../../../node/mock/lib/secp256k1.mjs';
import { checkpointDigest, stateRootOfForTest, CHECKPOINT_FIELDS } from '../../../node/mock/conformance/run.mjs';
import { nodeDevCommittee } from '../../lib/keys.mjs';

/** cast tuple literal of a checkpoint as the API serves it, in VAULT.md 5.2 field order. */
export const checkpointTuple = (C) => `(${CHECKPOINT_FIELDS.map((f) => String(C[f.name])).join(',')})`;
export const CHECKPOINT_SIG = `submitCheckpoint((${CHECKPOINT_FIELDS.map((f) => f.type).join(',')}),bytes[])`;

/** Submits the node's latest final checkpoint to the vault from a throwaway relayer (anyone may). */
export async function relayLatest(ctx, { minHeight = 0 } = {}) {
  const { api, chain, stack } = ctx;
  const C = await api.until(async () => { const r = await api.get('/v1/finality/latest'); return r.status === 200 && r.body.height >= minHeight ? r.body : null; }, { ms: 60000, what: `a final checkpoint at height >= ${minHeight}` });
  const latest = await chain.call(stack.vault, 'checkpointCount()(uint64)');
  const r = await chain.send(ctx.who('maker').address, stack.vault, CHECKPOINT_SIG, [checkpointTuple(C), `[${C.signatures.map((s) => s.signature).join(',')}]`]);
  return { C, r, seqBefore: Number(latest[0]) };
}

export default async function (ctx) {
  const { api, stack, chain } = ctx;
  let C;
  await ctx.check('F-01', 'a checkpoint is final: its digest (VAULT.md 5.2, vault domain) and its state root (SPEC 11) recomputed independently', async () => {
    C = await api.until(async () => { const r = await api.get('/v1/finality/latest'); return r.status === 200 ? r.body : null; }, { ms: 60000, what: 'a final checkpoint' });
    const d = checkpointDigest(stack.vault, C);
    expect(d === C.digest, `digest ${d} vs ${C.digest}`);
    expect(stateRootOfForTest(C) === C.stateRoot, 'state root equation');
    return { height: C.height, digest: C.digest, stateRoot: C.stateRoot };
  });
  await ctx.check('F-02', 'it carries at least 5 signatures, each recovering to a distinct committee member, ascending', async () => {
    const committee = await api.json('/v1/committee');
    const who = C.signatures.map((s) => recover(unhex(C.digest), unhex(s.signature)));
    expect(who.every((w, i) => w === C.signatures[i].validator && committee.validators.includes(w)), 'a signature does not recover to a committee member');
    expect(new Set(who).size === who.length && who.length >= committee.threshold, `${who.length} signatures`);
    expect(who.every((w, i) => i === 0 || w > who[i - 1]), 'not ascending');
    return { signers: who };
  });
  await ctx.check('F-03', 'independent replay: the engine CLI (a separate binary) replays the published log from genesis and reproduces the state root and log hash of every block', async () => {
    const signers = stack.oracleCommittee ?? nodeDevCommittee(stack.seed, 'oracle', 7).map((k) => k.address);
    const out = JSON.parse(await ctx.host('replay-check.sh', stack.label, stack.hostApi, stack.vault, signers.join(','), stack.oracleQuorum ?? 5, stack.fundingIntervalMs ?? 0));
    expect(out.ok, JSON.stringify(out));
    expect(out.lastHeight >= C.height, `replayed to ${out.lastHeight}, checkpoint at ${C.height}`);
    return out;
  });
  await ctx.check('F-04', 'the vault accepts the checkpoint with the committee\'s signatures and computes the same state root', async () => {
    blockedIf(!chain, 'fake L2 stand-in: no vault');
    const { C: cp, r, seqBefore } = await relayLatest(ctx);
    expect(r.ok, `submitCheckpoint: ${r.error}`);
    const seq = Number(await chain.callOne(stack.vault, 'checkpointCount()(uint64)'));
    expect(seq === seqBefore + 1, `checkpointCount ${seqBefore} -> ${seq}`);
    const root = await chain.callOne(stack.vault, `stateRootOf((${CHECKPOINT_FIELDS.map((f) => f.type).join(',')}))(bytes32)`, [checkpointTuple(cp)]);
    expect(root === cp.stateRoot, `vault ${root} vs node ${cp.stateRoot}`);
    ctx.state.lastRelayed = cp;
    return { seq, height: cp.height, gas: r.gas, tx: r.tx, stateRoot: root };
  });
  await ctx.check('F-05', 'the vault refuses the next checkpoint with one field changed, with 4 signatures, and a replay of the accepted one', async () => {
    blockedIf(!chain, 'fake L2 stand-in: no vault');
    const prev = ctx.state.lastRelayed;
    const cp = await api.until(async () => { const r = await api.get('/v1/finality/latest'); return r.status === 200 && r.body.height > prev.height ? r.body : null; }, { ms: 60000, what: 'a newer final checkpoint' });
    const sigs = cp.signatures.map((s) => s.signature);
    const from = ctx.who('maker').address;
    const tampered = { ...cp, withdrawalsTotal: String(BigInt(cp.withdrawalsTotal) + 1n) };
    const a = await chain.send(from, stack.vault, CHECKPOINT_SIG, [checkpointTuple(tampered), `[${sigs.join(',')}]`]);
    expect(!a.ok, 'a checkpoint with an altered field was accepted');
    const b = await chain.send(from, stack.vault, CHECKPOINT_SIG, [checkpointTuple(cp), `[${sigs.slice(0, 4).join(',')}]`]);
    expect(!b.ok, 'a checkpoint with 4 signatures was accepted');
    const c = await chain.send(from, stack.vault, CHECKPOINT_SIG, [checkpointTuple(prev), `[${prev.signatures.map((s) => s.signature).join(',')}]`]);
    expect(!c.ok, 'the already accepted checkpoint was accepted again');
    const seq = Number(await chain.callOne(stack.vault, 'checkpointCount()(uint64)'));
    return { tampered: a.error, fourSignatures: b.error, replay: c.error, checkpointCount: seq };
  });
}
