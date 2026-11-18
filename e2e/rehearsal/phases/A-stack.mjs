// Phase A: the stack is one system (node, committee, signing domain, vault, L2 view, oracle). UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';

export default async function (ctx) {
  const { api, stack, chain, deployment } = ctx;
  await ctx.check('A-01', 'node answers in dev mode, declared unaudited, sequencer running, committee 5 of 7', async () => {
    const s = await api.json('/v1/status');
    expect(s.mode === 'dev' && s.unaudited === true, `mode ${s.mode}, unaudited ${s.unaudited}`);
    expect(s.sequencer.state === 'running', `sequencer ${s.sequencer.state}`);
    expect(s.committee.n === 7 && s.committee.threshold === 5, JSON.stringify(s.committee));
    return { version: s.version, apiVersion: s.apiVersion, height: s.latestBlock?.height, committee: s.committee };
  });
  await ctx.check('A-02', 'the engine signing domain is bound to the vault of this stack (SPEC 2.1)', async () => {
    const d = api.eip.domain;
    expect(d.name === 'PosternPerps' && d.version === '1' && d.chainId === 700770, JSON.stringify(d));
    expect(d.verifyingContract.toLowerCase() === stack.vault.toLowerCase(), `${d.verifyingContract} vs ${stack.vault}`);
    return d;
  });
  await ctx.check('A-03', 'the vault on the L2 is the real PerpsVault with the node\'s committee (5 of 7), the owner\'s fee recipient and production-shape delays', async () => {
    blockedIf(!chain, 'fake L2 stand-in: no vault contract in this stack');
    const committee = await api.json('/v1/committee');
    expect(Number(await chain.callOne(stack.vault, 'threshold()(uint8)')) === committee.threshold, 'threshold');
    expect(Number(await chain.callOne(stack.vault, 'validatorCount()(uint256)')) === committee.validators.length, 'validator count');
    for (let i = 0; i < committee.validators.length; i++) {
      const v = (await chain.callOne(stack.vault, 'validators(uint256)(address)', [i])).toLowerCase();
      expect(v === committee.validators[i], `validator ${i}: vault ${v}, node ${committee.validators[i]}`);
    }
    const fee = (await chain.callOne(stack.vault, 'feeRecipient()(address)')).toLowerCase();
    expect(fee === '0x775a993f09bd80aecf8f8c2ade0c84158e3a0544', `fee recipient ${fee}`);
    const forcedDelay = Number(await chain.callOne(stack.vault, 'forcedDelay()(uint64)'));
    const escapeDelay = Number(await chain.callOne(stack.vault, 'escapeDelay()(uint64)'));
    expect(forcedDelay >= 86400 && escapeDelay >= forcedDelay, `delays ${forcedDelay} / ${escapeDelay}`);
    expect(chain.chainId === 700770, `chain id ${chain.chainId}`);
    expect((await chain.callOne(stack.vault, 'token()(address)')).toLowerCase() === stack.token.toLowerCase(), 'token');
    return { vault: stack.vault, token: stack.token, feeRecipient: fee, forcedDelay, escapeDelay, validators: committee.validators, deployBlock: deployment?.deployBlock };
  });
  await ctx.check('A-04', 'the node reads the L2 from two endpoints that agree', async () => {
    const s = await api.until(async () => { const x = await api.json('/v1/status'); return x.l2.endpointsAgree ? x : null; }, { ms: 30000, what: 'l2.endpointsAgree' });
    return s.l2;
  });
  await ctx.check('A-05', 'every market has a live committee index price and is active', async () => {
    const out = {};
    for (const m of api.markets) {
      const t = await api.until(async () => { const x = await api.json(`/v1/markets/${m.symbol}/ticker`); return Number(x.indexPrice) > 0 ? x : null; }, { ms: 20000, what: `${m.symbol} index price` });
      const mm = await api.json(`/v1/markets/${m.symbol}`);
      expect(mm.status === 'active', `${m.symbol} is ${mm.status}`);
      out[m.symbol] = { index: t.indexPrice, mark: t.markPrice };
    }
    blockedIf(stack.oracle === 'dev', 'dev oracle stand-in (fixed prices signed in-process); passes only with the replay feeder: observed ' + JSON.stringify(out));
    return out;
  });
}
