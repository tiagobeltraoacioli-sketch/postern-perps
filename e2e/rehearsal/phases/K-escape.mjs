// Phase K (LAST: escape is one way): the committee goes silent, the vault enters escape after escapeDelay, every
// account leaves with a Merkle proof against the last accepted checkpoint, system shares last. UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { usdc, sleep } from '../../lib/client.mjs';
import { fmtUsdc, SYSTEM, OWNER_FEE_ADDRESS } from '../util.mjs';
import { user } from '../../lib/keys.mjs';
import { unhex } from '../../../node/mock/lib/keccak.mjs';
import { leafHash, root32 } from '../../../node/mock/lib/merkle32.mjs';
import { relayLatest, CHECKPOINT_SIG, checkpointTuple } from './F-finality.mjs';

const ACCOUNT_T = '(address,uint32,int128,uint64,bytes32)';
const leafTuple = (l) => `(${l.account},${l.index},${l.balance},${l.escapeValue},${l.positionsHash})`;
const LATEST = 'latest()(uint64,uint64,uint64,uint64,uint64,uint64,uint128,bytes32,bytes32)';

export default async function (ctx) {
  const { api, chain, stack, CAST } = ctx;
  if (!chain) { await ctx.blocked('K-01', 'validators stop, escape, claims sum to custody with insurance first', 'fake L2 stand-in: no vault'); return; }
  const anyone = user('relayer').address;
  const tokenBal = async (a) => BigInt(await chain.callOne(stack.token, 'balanceOf(address)(uint256)', [a]));
  const view = async (sig, args = []) => chain.callOne(stack.vault, sig, args);
  const send = (sig, args = [], from = anyone) => chain.send(from, stack.vault, sig, args);
  const all = [...CAST.map((c) => ({ label: c.label, address: c.address })), ...Object.entries(SYSTEM).map(([label, address]) => ({ label, address }))];
  let H, proofs = {}, cp, totalEscape, lastCp;

  await ctx.check('K-01', 'before the silence: the vault holds the node\'s latest final checkpoint, and every account\'s escape leaf and 32 siblings for THAT checkpoint are fetched and verified against the root the vault stores', async () => {
    for (const c of CAST.filter((x) => x.key)) await api.cancelAll(c);   // no resting orders: every account as it will escape
    await sleep(1500);
    const h0 = (await api.json('/v1/status')).latestBlock.height;
    await api.until(async () => {
      await relayLatest(ctx, { minHeight: h0 });   // the rehearsal's relay or the node's own relayer: either may deliver it
      return Number((await chain.call(stack.vault, LATEST))[0]) >= h0 ? true : null;
    }, { ms: 90000, every: 2000, what: 'the vault at a checkpoint taken after every order was cancelled' });
    const L = await chain.call(stack.vault, LATEST);
    H = Number(L[0]); totalEscape = BigInt(L[6]);
    lastCp = await api.json(`/v1/finality/${H}`);
    expect(lastCp.height === H && lastCp.stateRoot === L[8], `the node's checkpoint at ${H} is not the one the vault holds`);
    let sum = 0n;
    for (const a of all) {
      const r = await api.get(`/v1/accounts/${a.address}/proof?height=${H}`);
      if (r.status !== 200) continue;   // an address the engine never credited has no leaf
      const p = r.body;
      expect(p.checkpoint.height === H && p.checkpoint.accountsRoot === L[7], `${a.label}: proof for checkpoint ${p.checkpoint.height}, vault holds ${H}`);
      expect(root32(leafHash(unhex(p.leafData.encoded)), p.index, p.proof) === L[7], `${a.label}: the siblings do not reach the vault's accountsRoot`);
      proofs[a.label] = p; sum += BigInt(p.leafData.escapeValue);
    }
    expect(sum === totalEscape, `the leaves fetched sum to ${fmtUsdc(sum)}, the checkpoint's totalEscape is ${fmtUsdc(totalEscape)}: an account is missing`);
    cp = lastCp;
    return { checkpointHeight: H, accounts: Object.keys(proofs).length, totalEscape: fmtUsdc(totalEscape), escapeValues: Object.fromEntries(Object.entries(proofs).map(([k, p]) => [k, fmtUsdc(BigInt(p.leafData.escapeValue))])) };
  });

  await ctx.check('K-02', 'the exchange goes silent: the sequencer (with the validators inside it), the oracle feeder and all separate validators but one are stopped', async () => {
    expect(H, 'K-01 failed');
    for (let i = stack.localValidators; i < 7; i++) if (!(stack.validatorApi && i === stack.validatorApiIndex)) await ctx.host('svc.sh', 'stop', `validator-${i}`, 'TERM');
    await ctx.host('svc.sh', 'stop', 'feeder', 'TERM');
    await ctx.host('svc.sh', 'stop', 'node', 'INT');
    const down = await api.get('/v1/status').then(() => false, () => true);
    expect(down, 'the API still answers');
    return { hostServices: (await ctx.host('svc.sh', 'status')).split('\n') };
  });
  await ctx.check('K-02b', 'WITHOUT the sequencer: one validator\'s own read service (its own log, its own replay) serves every account\'s escape leaf and siblings for the checkpoint the vault holds; they verify against the vault\'s root and equal the ones fetched before (R436-11)', async () => {
    blockedIf(!stack.validatorApi, 'no validator read service in this stack (up.sh --validator-read off, or all validators inside the sequencer): the proofs used below were fetched from the sequencer before it stopped');
    const L = await chain.call(stack.vault, LATEST);
    let n = 0;
    try {
      for (const a of all) {
        if (!proofs[a.label]) continue;
        const r = await fetch(`${stack.validatorApi}/v1/accounts/${a.address}/proof?height=${H}`);
        expect(r.status === 200, `${a.label}: validator answered HTTP ${r.status}`);
        const p = await r.json();
        const sib = p.proof ?? p.siblings;
        expect(root32(leafHash(unhex(p.leafData.encoded)), p.index, sib) === L[7], `${a.label}: the validator's siblings do not reach the vault's accountsRoot`);
        expect(p.leafData.encoded === proofs[a.label].leafData.encoded && JSON.stringify(sib) === JSON.stringify(proofs[a.label].proof), `${a.label}: the validator's proof differs from the sequencer's`);
        proofs[a.label] = { ...proofs[a.label], leafData: p.leafData, proof: sib, index: p.index, from: 'validator' };   // the claims below use the validator's
        n++;
      }
    } finally {
      await ctx.host('svc.sh', 'stop', `validator-${stack.validatorApiIndex}`, 'TERM');
    }
    return { validator: stack.validatorApiIndex, proofsServedWithoutTheSequencer: n, checkpointHeight: H };
  });

  const esc = ctx.who('escapee');
  let lateIndex, custody0, t0 = {};
  await ctx.check('K-03', 'a deposit made after the last checkpoint is accepted by the vault and never credited by anyone', async () => {
    const units = String(usdc('500'));
    let r = await chain.send(esc.address, stack.token, 'mint(address,uint256)', [esc.address, units]); expect(r.ok, r.error);
    r = await chain.send(esc.address, stack.token, 'approve(address,uint256)', [stack.vault, units]); expect(r.ok, r.error);
    r = await send('deposit(uint64,address)', [units, esc.address], esc.address); expect(r.ok, `deposit: ${r.error}`);
    lateIndex = Number(await view('depositCount()(uint64)')) - 1;
    const credited = Number((await chain.call(stack.vault, LATEST))[2]);
    expect(lateIndex >= credited, `deposit ${lateIndex} is below the credited count ${credited}`);
    return { depositIndex: lateIndex, creditedByTheLastCheckpoint: credited };
  });
  await ctx.check('K-04', 'escape cannot be triggered while the committee is merely late; after escapeDelay without a checkpoint anyone triggers it', async () => {
    const early = await send('triggerEscape()');
    expect(!early.ok && Number(await view('escapeReason()(uint8)')) === 0, `triggered early: ${JSON.stringify(early).slice(0, 200)}`);
    const delay = Number(await view('escapeDelay()(uint64)'));
    await chain.warp(delay - 3600);
    const stillEarly = await send('triggerEscape()');
    expect(!stillEarly.ok, 'triggered one hour before the delay');
    await chain.warp(3700);
    expect(Number(await view('escapeReason()(uint8)')) === 1, 'escapeReason is not 1 (silence)');
    custody0 = BigInt(await view('custody()(uint64)'));
    const r = await send('triggerEscape()');
    expect(r.ok && (await view('inEscape()(bool)')) === 'true', `triggerEscape: ${r.error}`);
    return { escapeDelaySeconds: delay, early: early.error, oneHourBefore: stillEarly.error, tx: r.tx, custodyAtTrigger: fmtUsdc(custody0), escapePool: fmtUsdc(BigInt(await view('escapePool()(uint64)'))) };
  });
  await ctx.check('K-05', 'in escape the vault takes no deposit, no forced request and no checkpoint (not even the committee\'s own last one)', async () => {
    const d = await send('deposit(uint64,address)', [String(usdc('10')), esc.address], esc.address);
    const f = await send('requestForced(uint8,uint64)', [0, 0], esc.address);
    const c = await send(CHECKPOINT_SIG, [checkpointTuple(cp), `[${cp.signatures.map((s) => s.signature).join(',')}]`]);
    expect(!d.ok && !f.ok && !c.ok, `deposit ${d.ok}, forced ${f.ok}, checkpoint ${c.ok}`);
    return { deposit: d.error, forced: f.error, checkpoint: c.error };
  });

  let senior, ins, back;
  const claim = (p, want) => send(`claimEscape(${ACCOUNT_T},bytes32[],uint64)`, [leafTuple(p.leafData), `[${p.proof.join(',')}]`, want]);
  await ctx.check('K-06', 'no claim before the system shares are proven; once proven, users are senior: the payable amount is every user\'s escape value in full, the insurance and backstop shares wait behind them', async () => {
    const first = proofs.alice;
    const before = await claim(first, first.leafData.escapeValue);
    expect(!before.ok, 'a claim was paid before proveSystemShares');
    ins = BigInt(proofs.insurance.leafData.escapeValue); back = BigInt(proofs.backstop.leafData.escapeValue);
    const r = await send(`proveSystemShares(${ACCOUNT_T},bytes32[],${ACCOUNT_T},bytes32[])`, [leafTuple(proofs.insurance.leafData), `[${proofs.insurance.proof.join(',')}]`, leafTuple(proofs.backstop.leafData), `[${proofs.backstop.proof.join(',')}]`]);
    expect(r.ok, `proveSystemShares: ${r.error}`);
    senior = totalEscape - ins - back;
    const payable = BigInt(await view('escapePayable()(uint64)')), pool = BigInt(await view('escapePool()(uint64)'));
    expect(payable === (pool < senior ? pool : senior), `escapePayable ${fmtUsdc(payable)}, pool ${fmtUsdc(pool)}, senior ${fmtUsdc(senior)}`);
    expect(payable === senior, `the pool does not cover the users in full: pool ${fmtUsdc(pool)}, users ${fmtUsdc(senior)}`);
    const sys = await claim(proofs.insurance, proofs.insurance.leafData.escapeValue);
    expect(!sys.ok, 'the insurance account claimed like a user');
    return { claimBeforeProof: before.error, pool: fmtUsdc(pool), usersSenior: fmtUsdc(senior), insuranceShare: fmtUsdc(ins), backstopShare: fmtUsdc(back), escapePayable: fmtUsdc(payable), insuranceClaim: sys.error };
  });

  const paid = {};
  await ctx.check('K-07', 'every user claims with its proof and receives exactly its escape value (large claims after the large-amount delay, in window-limited parts); the fee account\'s share goes only to the owner\'s address; no claim pays twice', async () => {
    expect(senior !== undefined, 'K-06 failed');
    const users = Object.entries(proofs).filter(([k]) => !['insurance', 'backstop'].includes(k));
    for (const [label, p] of users) {
      const ev = BigInt(p.leafData.escapeValue);
      const to = label === 'fee' ? OWNER_FEE_ADDRESS : p.address;
      const b0 = await tokenBal(to);
      if (ev === 0n) { paid[label] = 0n; continue; }
      const r = await claim(p, ev);
      expect(r.ok, `${label}: claimEscape: ${r.error}`);
      const key = await view('escapeKey(address)(bytes32)', [p.address]);
      for (let day = 0; day < 8 && (await tokenBal(to)) - b0 < ev; day++) {   // a large payout: delay, then the daily window
        await chain.warp(86400 + 5);
        await send('release(bytes32,uint64)', [key, String(ev - ((await tokenBal(to)) - b0))]);
      }
      const got = (await tokenBal(to)) - b0;
      expect(got === ev, `${label}: received ${fmtUsdc(got)}, escape value ${fmtUsdc(ev)}`);
      paid[label] = got;
      const again = await claim(p, ev);
      expect(!again.ok && (await tokenBal(to)) - b0 === ev, `${label}: a second claim moved tokens`);
    }
    const total = Object.values(paid).reduce((a, b) => a + b, 0n);
    expect(total === senior, `users were paid ${fmtUsdc(total)}, their escape values sum to ${fmtUsdc(senior)}`);
    return { paid: Object.fromEntries(Object.entries(paid).map(([k, v]) => [k, fmtUsdc(v)])), totalPaidToUsers: fmtUsdc(total) };
  });
  await ctx.check('K-08', 'the deposit nobody credited is refunded in full from the vault\'s own records, once', async () => {
    const b0 = await tokenBal(esc.address);
    const wrong = await send('refundDeposit(uint64,address,uint64)', [lateIndex, esc.address, String(usdc('501'))]);
    expect(!wrong.ok, 'a refund of another amount was paid');
    const r = await send('refundDeposit(uint64,address,uint64)', [lateIndex, esc.address, String(usdc('500'))]);
    expect(r.ok, `refundDeposit: ${r.error}`);
    const again = await send('refundDeposit(uint64,address,uint64)', [lateIndex, esc.address, String(usdc('500'))]);
    expect(!again.ok && (await tokenBal(esc.address)) - b0 === usdc('500'), `refund ${fmtUsdc((await tokenBal(esc.address)) - b0)}`);
    const credited = await send('refundDeposit(uint64,address,uint64)', [0, CAST[0].address, String(usdc(CAST[0].deposit))]);
    expect(!credited.ok, 'a deposit the checkpoint had credited was refunded');
    return { refunded: '500.000000', wrongAmount: wrong.error, second: again.error, creditedDeposit: credited.error };
  });
  await ctx.check('K-09', 'the books close: custody at the trigger = users paid + refund + what the vault still holds; what it holds is the subordinated system share (insurance + backstop), not releasable before 90 days', async () => {
    const left = await tokenBal(stack.vault);
    const users = Object.values(paid).reduce((a, b) => a + b, 0n);
    expect(custody0 === users + usdc('500') + left, `custody ${fmtUsdc(custody0)} vs paid ${fmtUsdc(users)} + refund 500 + left ${fmtUsdc(left)}`);
    const sub = BigInt(await view('subordinatedTotal()(uint128)'));
    const pool = BigInt(await view('escapePool()(uint64)'));
    const wantSub = ins + back < pool - senior ? ins + back : pool - senior;
    expect(sub === wantSub, `subordinated ${fmtUsdc(sub)}, expected min(insurance + backstop, pool - users) = ${fmtUsdc(wantSub)}`);
    expect(left >= sub && left - sub < 1000n, `the vault holds ${fmtUsdc(left)}, subordinated share ${fmtUsdc(sub)}`);
    const sweep = await send('sweepEscapeSurplus()');
    expect(!sweep.ok, 'the surplus was swept before the 90 days');
    const o0 = await tokenBal(OWNER_FEE_ADDRESS);
    const sysKey = await view('escapeKey(address)(bytes32)', [SYSTEM.insurance]);
    const early = await send('release(bytes32,uint64)', [sysKey, String(sub)]);
    expect((await tokenBal(OWNER_FEE_ADDRESS)) === o0, 'the system share was released before the 90 days');
    return { custodyAtTrigger: fmtUsdc(custody0), paidToUsers: fmtUsdc(users), refund: '500.000000', stillInVault: fmtUsdc(left), subordinatedSystemShare: fmtUsdc(sub), sweepBefore90Days: sweep.error, systemReleaseBefore90Days: early.error ?? 'no payment' };
  });
}
