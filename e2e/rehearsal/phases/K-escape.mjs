// Phase K (LAST: escape is one way): the committee goes silent, the vault enters escape after escapeDelay, every
// account leaves with a Merkle proof against the last accepted checkpoint, system shares last. UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { usdc, sleep } from '../../lib/client.mjs';
import { fmtUsdc, SYSTEM, OWNER_FEE_ADDRESS, onTick } from '../util.mjs';
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
  // REHEARSAL_ESCAPE=forced (R436-19): the exchange stays ALIVE but never sees a user's forced request (one L2 endpoint
  // hides the log, so the two-endpoint watcher of sequencer and validators makes no progress); the request passes its
  // deadline, the vault refuses the next checkpoint and anyone triggers escape with reason 2. Default: silence (reason 1).
  const FORCED = process.env.REHEARSAL_ESCAPE === 'forced';
  // REHEARSAL_ESCAPE=shortfall: before the silence an account goes UNDER WATER where the engine may not liquidate it
  // (it also holds a position in a market that is halted), with an insurance fund far smaller than the deficit: the
  // last checkpoint then promises users more than the vault holds, and the escape must pay them pro rata, first.
  const SHORT = process.env.REHEARSAL_ESCAPE === 'shortfall';
  const anyone = user('relayer').address;
  const tokenBal = async (a) => BigInt(await chain.callOne(stack.token, 'balanceOf(address)(uint256)', [a]));
  const view = async (sig, args = []) => chain.callOne(stack.vault, sig, args);
  const send = (sig, args = [], from = anyone) => chain.send(from, stack.vault, sig, args);
  const all = [...CAST.map((c) => ({ label: c.label, address: c.address })), ...Object.entries(SYSTEM).map(([label, address]) => ({ label, address }))];
  // the account the PAGE deposited from in phase P (anvil's development account #9), when the engine knows it
  try { const dev = await chain.rpc('eth_accounts'); for (const i of [8, 9]) { const page = dev[i]?.toLowerCase(); if (page && !all.some((a) => a.address === page)) all.push({ label: `page-${i}`, address: page }); } } catch { /* no such account */ }
  let H, proofs = {}, cp, totalEscape, lastCp;

  if (SHORT) await ctx.check('K-00s', 'setup of the shortfall: a 20x long in BTC that also holds ETH; ETH halts (its oracle price is withheld), BTC falls 15%: the account is under water, cannot be liquidated, and the checkpoint\'s escape values exceed what the vault holds', async () => {
    blockedIf(stack.oracle !== 'replay', 'needs the scripted oracle');
    const btc = api.market('BTC-USD'), eth = api.market('ETH-USD');
    const maker = ctx.who('maker'), v = ctx.who('liq-adl');
    const t = (await api.json('/v1/tickers')).tickers;
    const ix = (sym) => t.find((x) => x.symbol === sym).indexPrice;
    const px0 = (m, bps) => onTick(m, ix(m.symbol), bps);   // exact, on the market's tick
    await api.cancelAll(maker);
    let r = await api.act(v, 'UpdateLeverage', { marketId: btc.marketId, leverage: 20, isolated: false }); expect(r.body?.result?.ok, JSON.stringify(r.body));
    r = await api.order(maker, btc, { buy: false, price: px0(btc, 10), size: '1', tif: 2 }); expect(r.body?.result?.ok, JSON.stringify(r.body));
    r = await api.order(maker, eth, { buy: false, price: px0(eth, 10), size: '1', tif: 2 }); expect(r.body?.result?.ok, JSON.stringify(r.body));
    r = await api.order(v, eth, { buy: true, price: px0(eth, 10), size: eth.minOrderSize, tif: 1 }); expect(r.body?.result?.orderStatus === 'filled', `ETH: ${JSON.stringify(r.body)}`);
    r = await api.order(v, btc, { buy: true, price: px0(btc, 10), size: '0.4', tif: 1 }); expect(r.body?.result?.orderStatus === 'filled', `BTC: ${JSON.stringify(r.body)}`);
    await api.cancelAll(maker);
    const sol = ix('SOL-USD'), b0 = ix('BTC-USD');
    const target = onTick(btc, b0, -1500);
    // ETH is no longer priced (STALE halt after 10 s); BTC goes down in 40 rounds of about 40 bps
    await ctx.feed('shortfall', [{ p: { 1: b0, 3: sol }, absent: [2] }, { ramp: { 1: target }, rounds: 40 }, { p: {}, repeat: 100000 }]);
    await api.until(async () => { await api.cancelAll(ctx.who('alice')); return (await api.json('/v1/markets/ETH-USD')).status === 'halted'; }, { ms: 90000, every: 1500, what: 'ETH-USD halted' });
    const under = await api.until(async () => { const a = await api.account(v.address); return Number(a.equity) < 0 && Math.abs(Number((await api.json('/v1/markets/BTC-USD/ticker')).indexPrice) - Number(target)) < 1 ? a : null; }, { ms: 240000, every: 3000, what: 'the account under water at the end of the fall' });
    expect(under.positions.length === 2, `the account was liquidated or lost a position: ${JSON.stringify(under.positions.map((p) => [p.symbol, p.size]))}`);
    const ins = await api.account(SYSTEM.insurance);
    expect(Number(ins.equity) < -Number(under.equity), `the insurance fund (${ins.equity}) covers the deficit (${under.equity}): no shortfall`);
    ctx.state.shortfall = { account: v.address, equity: under.equity, insurance: ins.equity };
    return { victimEquity: under.equity, insuranceFund: ins.equity, btcFrom: b0, btcTo: target, ethStatus: 'halted', victimPositions: under.positions.map((p) => [p.symbol, p.size]) };
  });
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

  if (FORCED) {
    const victim = ctx.who('escapee');
    let reqBlock;
    await ctx.check('K-02f', 'a forced withdrawal is posted on the L2 while one L2 endpoint hides it: the exchange keeps running and never receives it', async () => {
      expect(H, 'K-01 failed');
      const set = await fetch(`${stack.l2Proxy}/__fault`, { method: 'POST', body: JSON.stringify({ mode: 'drop' }) });
      expect(set.status === 200, 'proxy fault');
      const r = await send('requestForced(uint8,uint64)', [2, 0], victim.address);
      expect(r.ok, `requestForced: ${r.error}`);
      reqBlock = r.block;
      await chain.waitFinalized(r.block);
      await sleep(8000);
      const f = (await api.json(`/v1/forced?account=${victim.address}`)).forced;
      const index = Number(await view('forcedCount()(uint64)')) - 1;
      expect(!f.some((x) => x.forcedId === String(index)), `the node received the request: ${JSON.stringify(f).slice(0, 200)}`);
      const s0 = await api.json('/v1/status');
      await api.cancelAll(ctx.who('alice'));
      const s1 = await api.until(async () => { const x = await api.json('/v1/status'); return x.latestBlock.height > s0.latestBlock.height ? x : null; }, { ms: 20000, what: 'a new block' });
      return { forcedIndex: index, l2Block: r.block, deadline: Number(await view('forcedDeadline(uint64)', [index])), nodeStillProducing: s1.latestBlock.height, nodeL2View: (await api.json('/v1/node')).l2.endpointsAgree };
    });
    await ctx.check('K-02g', 'past the deadline the vault refuses every further checkpoint (it would leave the request unprocessed): the censoring exchange cannot continue', async () => {
      const index = Number(await view('forcedCount()(uint64)')) - 1;
      const early = await send('triggerEscape()');
      expect(!early.ok && Number(await view('escapeReason()(uint8)')) === 0, 'escape was due before the deadline');
      await chain.warp(Number(await view('forcedDelay()(uint64)')) + 60);
      expect(Number(await view('escapeReason()(uint8)')) === 2, `escapeReason ${await view('escapeReason()(uint8)')}`);
      await api.cancelAll(ctx.who('alice'));
      const C = await api.until(async () => { const r = await api.get('/v1/finality/latest'); return r.status === 200 && r.body.height > H ? r.body : null; }, { ms: 60000, what: 'a newer final checkpoint' });
      const c = await send(CHECKPOINT_SIG, [checkpointTuple(C), `[${C.signatures.map((x) => x.signature).join(',')}]`]);
      expect(!c.ok && Number((await chain.call(stack.vault, LATEST))[0]) === H, `a checkpoint that skips the overdue request was accepted (vault at ${(await chain.call(stack.vault, LATEST))[0]})`);
      return { forcedIndex: index, refusedCheckpointHeight: C.height, refusal: c.error, vaultStillAt: H, earlyTrigger: early.error };
    });
  }
  if (!FORCED) await ctx.check('K-02', 'the exchange goes silent: the sequencer (with the validators inside it), the oracle feeder and all separate validators but one are stopped', async () => {
    expect(H, 'K-01 failed');
    for (let i = stack.localValidators; i < 7; i++) if (!(stack.validatorApi && i === stack.validatorApiIndex)) await ctx.host('svc.sh', 'stop', `validator-${i}`, 'TERM');
    await ctx.host('svc.sh', 'stop', 'feeder', 'TERM');
    await ctx.host('svc.sh', 'stop', 'node', 'INT');
    const down = await api.get('/v1/status').then(() => false, () => true);
    expect(down, 'the API still answers');
    return { hostServices: (await ctx.host('svc.sh', 'status')).split('\n') };
  });
  await ctx.check('K-02b', (FORCED ? 'not from the sequencer' : 'WITHOUT the sequencer') + ': one validator\'s own read service (its own log, its own replay) serves every account\'s escape leaf and siblings for the checkpoint the vault holds; they verify against the vault\'s root and equal the ones fetched before (R436-11)', async () => {
    if (FORCED) expect((await api.get('/v1/status')).status === 200, 'the sequencer was expected to be alive in this variant');
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
      if (!FORCED) await ctx.host('svc.sh', 'stop', `validator-${stack.validatorApiIndex}`, 'TERM');
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
  if (FORCED) await ctx.check('K-04f', 'anyone triggers escape for the overdue forced request (reason 2), with the exchange still running', async () => {
    expect(Number(await view('escapeReason()(uint8)')) === 2, 'escape is not due for an overdue forced request');
    custody0 = BigInt(await view('custody()(uint64)'));
    const r = await send('triggerEscape()');
    expect(r.ok && (await view('inEscape()(bool)')) === 'true', `triggerEscape: ${r.error}`);
    await fetch(`${stack.l2Proxy}/__fault`, { method: 'POST', body: JSON.stringify({ mode: 'none' }) });
    return { reason: 2, tx: r.tx, custodyAtTrigger: fmtUsdc(custody0), escapePool: fmtUsdc(BigInt(await view('escapePool()(uint64)'))), sequencerAlive: (await api.get('/v1/status')).status === 200 };
  });
  else await ctx.check('K-04', 'escape cannot be triggered while the committee is merely late; after escapeDelay without a checkpoint anyone triggers it', async () => {
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
    if (SHORT) {
      expect(pool < senior && payable === pool, `no shortfall at the vault: pool ${fmtUsdc(pool)}, users ${fmtUsdc(senior)}, payable ${fmtUsdc(payable)}`);
      ctx.state.haircutPpm = Number((senior - pool) * 1000000n / senior);
    } else expect(payable === senior, `the pool does not cover the users in full: pool ${fmtUsdc(pool)}, users ${fmtUsdc(senior)}`);
    ctx.state.escape = { payable, senior };
    const sys = await claim(proofs.insurance, proofs.insurance.leafData.escapeValue);
    expect(!sys.ok, 'the insurance account claimed like a user');
    return { claimBeforeProof: before.error, pool: fmtUsdc(pool), usersSenior: fmtUsdc(senior), insuranceShare: fmtUsdc(ins), backstopShare: fmtUsdc(back), escapePayable: fmtUsdc(payable), insuranceClaim: sys.error };
  });

  const paid = {};
  await ctx.check('K-07', 'every user claims with its proof and receives exactly its escape value (large claims after the large-amount delay, in window-limited parts); the fee account\'s share goes only to the owner\'s address; no claim pays twice', async () => {
    expect(senior !== undefined, 'K-06 failed');
    const users = Object.entries(proofs).filter(([k]) => !['insurance', 'backstop'].includes(k));
    for (const [label, p] of users) {
      const full = BigInt(p.leafData.escapeValue);
      // VAULT.md 7: floor(escapeValue x escapePayable / senior): the whole value whenever the pool covers the users
      const ev = (full * ctx.state.escape.payable) / ctx.state.escape.senior;
      const to = label === 'fee' ? OWNER_FEE_ADDRESS : p.address;
      const b0 = await tokenBal(to);
      if (ev === 0n) { paid[label] = 0n; continue; }
      const r = await claim(p, full);
      expect(r.ok, `${label}: claimEscape: ${r.error}`);
      const key = await view('escapeKey(address)(bytes32)', [p.address]);
      for (let day = 0; day < 8 && (await tokenBal(to)) - b0 < ev; day++) {   // a large payout: delay, then the daily window
        await chain.warp(86400 + 5);
        await send('release(bytes32,uint64)', [key, String(ev - ((await tokenBal(to)) - b0))]);
      }
      const got = (await tokenBal(to)) - b0;
      expect(got === ev, `${label}: received ${fmtUsdc(got)}, escape value ${fmtUsdc(ev)}`);
      paid[label] = got;
      const again = await claim(p, full);
      expect(!again.ok && (await tokenBal(to)) - b0 === ev, `${label}: a second claim moved tokens`);
    }
    const total = Object.values(paid).reduce((a, b) => a + b, 0n);
    const P = ctx.state.escape.payable;
    // every share is rounded DOWN: together they are the payable amount less at most one unit per claimant
    expect(total <= P && P - total <= BigInt(users.length), `users were paid ${fmtUsdc(total)}, the payable amount is ${fmtUsdc(P)} (users' escape values ${fmtUsdc(senior)})`);
    if (SHORT) expect(total < senior, 'no haircut although the vault held less than the users were promised');
    return { paid: Object.fromEntries(Object.entries(paid).map(([k, v]) => [k, fmtUsdc(v)])), totalPaidToUsers: fmtUsdc(total), usersEscapeValues: fmtUsdc(senior), payable: fmtUsdc(P), haircutPpm: ctx.state.haircutPpm ?? 0 };
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
  await ctx.check('K-09', 'the books close: custody at the trigger = users paid + refund + what the vault still holds; what it holds is the subordinated system share (insurance + backstop, not releasable before 90 days) plus certified withdrawal records nobody collected yet', async () => {
    const left = await tokenBal(stack.vault);
    const users = Object.values(paid).reduce((a, b) => a + b, 0n);
    expect(custody0 === users + usdc('500') + left, `custody ${fmtUsdc(custody0)} vs paid ${fmtUsdc(users)} + refund 500 + left ${fmtUsdc(left)}`);
    const sub = BigInt(await view('subordinatedTotal()(uint128)'));
    const pool = BigInt(await view('escapePool()(uint64)'));
    const wantSub = ins + back < pool - senior ? ins + back : pool - senior;
    const wantSub2 = SHORT ? 0n : wantSub;   // in a shortfall the users take the whole pool: the system accounts get nothing
    expect(sub === wantSub2, `subordinated ${fmtUsdc(sub)}, expected ${fmtUsdc(wantSub2)}`);
    // certified withdrawal records nobody collected yet stay payable in full in escape (VAULT.md 7): they are part of
    // what the vault still holds (here: the 1 USDC record the conformance suite creates and never collects)
    const unpaid = BigInt((await chain.call(stack.vault, LATEST))[5]) - BigInt(await view('withdrawnTotal()(uint64)'));
    expect(left >= sub + unpaid && left - sub - unpaid < 1000n, `the vault holds ${fmtUsdc(left)} = subordinated share ${fmtUsdc(sub)} + uncollected withdrawal records ${fmtUsdc(unpaid)} + ${fmtUsdc(left - sub - unpaid)} (only rounding dust may remain)`);
    const sweep = await send('sweepEscapeSurplus()');
    expect(!sweep.ok, 'the surplus was swept before the 90 days');
    const o0 = await tokenBal(OWNER_FEE_ADDRESS);
    const sysKey = await view('escapeKey(address)(bytes32)', [SYSTEM.insurance]);
    const early = await send('release(bytes32,uint64)', [sysKey, String(sub)]);
    expect((await tokenBal(OWNER_FEE_ADDRESS)) === o0, 'the system share was released before the 90 days');
    return { custodyAtTrigger: fmtUsdc(custody0), paidToUsers: fmtUsdc(users), refund: '500.000000', stillInVault: fmtUsdc(left), subordinatedSystemShare: fmtUsdc(sub), uncollectedWithdrawalRecords: fmtUsdc(unpaid), dust: fmtUsdc(left - sub - unpaid), sweepBefore90Days: sweep.error, systemReleaseBefore90Days: early.error ?? 'no payment' };
  });
}
