// Phase I: committee faults and restarts. UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { sleep } from '../../lib/client.mjs';
import { hex, unhex } from '../../../node/mock/lib/keccak.mjs';
import { sign } from '../../../node/mock/lib/secp256k1.mjs';
import { checkpointDigest } from '../../../node/mock/conformance/run.mjs';
import { nodeDevCommittee, userKey } from '../../lib/keys.mjs';

const status = (api) => api.json('/v1/status');

export default async function (ctx) {
  const { api, stack } = ctx;
  const remote = stack.localValidators < 7;   // validators stack.localValidators..6 are separate processes
  const committee = nodeDevCommittee(stack.seed, 'validator', 7);
  const alice = ctx.who('alice');
  const tick = () => api.cancelAll(alice);   // an exit-only action: makes a block without touching any position

  await ctx.check('I-01', 'a signature by a key outside the committee is refused', async () => {
    const C = (await api.json('/v1/finality/latest'));
    const r = await api.post('/v1/finality/signatures', { height: C.height, signature: hex(sign(unhex(C.digest), userKey('stranger'))) });
    expect(r.status === 400, `${r.status} ${JSON.stringify(r.body)}`);
    return { http: r.status, error: r.body?.error };
  });
  await ctx.check('I-02', 'a committee member that signs a DIFFERENT state root is not counted: its signature is refused and never published', async () => {
    const C = await api.json('/v1/finality/latest');
    const rogue = committee.find((k) => !C.signatures.some((s) => s.validator === k.address)) ?? committee[6];
    const wrong = { ...C, accountsRoot: '0x' + 'ab'.repeat(32) };   // a member whose replay gave another account tree
    const d = checkpointDigest(stack.vault, wrong);
    expect(d !== C.digest, 'the altered checkpoint has the same digest');
    const r = await api.post('/v1/finality/signatures', { height: C.height, signature: hex(sign(unhex(d), rogue.key)) });
    expect(r.status === 400, `the sequencer accepted a signature over another root: ${r.status} ${JSON.stringify(r.body)}`);
    const again = await api.json(`/v1/finality/${C.height}`);
    expect(again.digest === C.digest && again.signatures.every((s) => s.signature !== hex(sign(unhex(d), rogue.key))), 'the published checkpoint changed');
    return { rogue: rogue.address, http: r.status, error: r.body?.error, height: C.height };
  });
  await ctx.check('I-03', 'with 3 of 7 validators stopped nothing new becomes final while blocks continue; finality resumes when one returns', async () => {
    blockedIf(!remote, 'all 7 validators run inside the sequencer process in this stack (needs 431\'s --local-validators and separate validator processes)');
    for (const i of [4, 5, 6]) await ctx.host('svc.sh', 'stop', `validator-${i}`, 'TERM');
    const s0 = await status(api);
    for (let i = 0; i < 25; i++) { await tick(); await sleep(400); }
    const s1 = await status(api);
    expect(s1.latestBlock.height >= s0.latestBlock.height + 20, `blocks ${s0.latestBlock.height} -> ${s1.latestBlock.height}`);
    // at most the checkpoint already being signed when they stopped may still complete
    const stalledAt = s1.latestFinalized?.height ?? 0;
    for (let i = 0; i < 25; i++) { await tick(); await sleep(400); }
    const s2 = await status(api);
    expect((s2.latestFinalized?.height ?? 0) === stalledAt, `finality moved with 4 validators: ${stalledAt} -> ${s2.latestFinalized?.height}`);
    await ctx.host('validator-start.sh', '--again', 4);
    const s3 = await api.until(async () => { await tick(); const x = await status(api); return (x.latestFinalized?.height ?? 0) > stalledAt ? x : null; }, { ms: 60000, every: 1000, what: 'finality with 5 validators' });
    ctx.state.validatorsDown = [5, 6];
    return { blocksWhileDown: s2.latestBlock.height - s0.latestBlock.height, finalStalledAt: stalledAt, finalAfterReturn: s3.latestFinalized.height };
  });
  await ctx.check('I-04', 'validators restarted mid-run catch up from the published log and sign again (7 of 7 online)', async () => {
    blockedIf(!remote, 'all 7 validators run inside the sequencer process in this stack');
    for (const i of ctx.state.validatorsDown ?? []) await ctx.host('validator-start.sh', '--again', i);
    const n = await api.until(async () => {
      await tick();
      const [s, node] = [await status(api), await api.json('/v1/node')];
      const up = node.validators.filter((v) => v.height >= s.latestBlock.height - 2 && v.latched === null);
      return up.length === 7 ? { height: s.latestBlock.height, validators: node.validators.map((v) => ({ address: v.address, height: v.height, signed: v.signed })) } : null;
    }, { ms: 90000, every: 1500, what: '7 validators at the head' });
    return n;
  });
  for (const [id, sig, how] of [['I-05', 'INT', 'stopped cleanly'], ['I-06', 'KILL', 'killed (SIGKILL, no shutdown)']]) {
    await ctx.check(id, `the sequencer is ${how} and restarted mid-run: it recovers the same chain, the log verifies, trading and finality continue`, async () => {
      for (let i = 0; i < 3; i++) await tick();
      const before = await status(api);
      const blk = await api.json(`/v1/blocks/${before.latestBlock.height}`);
      await ctx.host('svc.sh', 'stop', 'node', sig);
      const down = await api.get('/v1/status').then(() => false, () => true);
      expect(down, 'the API still answers after the stop');
      await ctx.host('node-start.sh', '--again');
      const after = await api.until(() => status(api), { ms: 60000, what: 'the node after restart' });
      expect(after.latestBlock.height >= before.latestBlock.height, `height went back: ${before.latestBlock.height} -> ${after.latestBlock.height}`);
      const same = await api.json(`/v1/blocks/${before.latestBlock.height}`);
      expect(same.logHash === blk.logHash && same.stateRoot === blk.stateRoot, `block ${before.latestBlock.height} differs after the restart`);
      const node = await api.json('/v1/node');
      const r = await tick();
      expect(r.status === 200 && r.body?.status === 'included', `action after restart: ${r.status} ${JSON.stringify(r.body)}`);
      const fin0 = after.latestFinalized?.height ?? 0;
      const fin = await api.until(async () => { await tick(); const x = await status(api); return (x.latestFinalized?.height ?? 0) > Math.max(fin0, before.latestBlock.height) ? x : null; }, { ms: 90000, every: 1000, what: 'a checkpoint above the restart height' });
      return { heightBefore: before.latestBlock.height, logHash: blk.logHash, recovery: node.recovery, finalAfter: fin.latestFinalized.height };
    });
  }
}
