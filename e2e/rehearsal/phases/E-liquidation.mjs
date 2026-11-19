// Phase E: three liquidations on a scripted oracle path (432's replay tape through 431's ingest):
//   E-1x through the book, E-2x by auto-deleveraging (backstop unfunded), E-3x through the funded backstop.
// Each victim opens 0.4 BTC long at 20x on 1,500 USDC; the index then falls about 3.8% in steps far below jumpBps.
// Balances are checked to the unit from the published fills and funding payments. UNAUDITED prototype.
import { expect, blockedIf } from '../check.mjs';
import { usdc, px, sz, sleep } from '../../lib/client.mjs';
import { bal, fmtUsdc, tradeFee, onTick, SYSTEM } from '../util.mjs';
import { user } from '../../lib/keys.mjs';

const SIZE = '0.4';
const LIQ_FEE_PPM = 5000;   // compliance/markets.json liquidationFeeBps 50

export default async function (ctx) {
  const { api, chain, stack } = ctx;
  if (stack.oracle !== 'replay') { await ctx.blocked('E-10', 'a liquidation through the book, one by ADL, one through the backstop', 'the stack runs the fixed-price dev oracle: no scripted price path'); return; }
  const btc = api.market('BTC-USD');
  const maker = ctx.who('maker');
  const ticker = () => api.json('/v1/markets/BTC-USD/ticker');
  const position = async (a) => (await api.account(a)).positions.find((p) => p.symbol === 'BTC-USD') ?? null;
  const others = async () => { const t = await api.json('/v1/tickers'); return Object.fromEntries(t.tickers.filter((x) => x.symbol !== 'BTC-USD').map((x) => [String(api.market(x.symbol).marketId), x.indexPrice])); };
  /** Moves the BTC index in a straight line to `to` over `rounds` oracle rounds (3 s each), then holds. */
  const ramp = async (name, to, rounds) => {
    const from = (await ticker()).indexPrice;
    const perRoundBps = Math.abs(Number(to) - Number(from)) / Number(from) / rounds * 10000;
    expect(perRoundBps < 400, `ramp of ${perRoundBps.toFixed(0)} bps per round would trip the jump halt`);
    await ctx.feed(name, [{ p: { 1: from, ...(await others()) } }, { ramp: { 1: to }, rounds }, { p: {}, repeat: 100000 }]);
    return { from, to, rounds, perRoundBps: Number(perRoundBps.toFixed(1)) };
  };
  /** Victim opens SIZE long at 20x against the maker's ask at `ask`; returns what the open cost. */
  const open = async (victim, ask) => {
    let r = await api.act(victim, 'UpdateLeverage', { marketId: btc.marketId, leverage: 20, isolated: false });
    expect(r.body?.result?.ok === true, `leverage 20: ${JSON.stringify(r.body)}`);
    r = await api.order(victim, btc, { buy: true, price: ask, size: SIZE, tif: 1 });
    expect(r.body?.result?.ok === true && r.body.result.orderStatus === 'filled', `open: ${JSON.stringify(r.body)}`);
    return { entry: ask, openFee: tradeFee(ask, SIZE, 450), t: (await api.json('/v1/status')).time };
  };
  const liquidated = (victim) => api.until(async () => ((await position(victim.address)) === null ? true : null), { ms: 240000, every: 1500, what: `${victim.label} liquidated` });
  /**
   * The victim's ledger from published data: balance = deposit - open fee + funding + realized - liquidation fee.
   * The liquidation fee itself is NOT published by the API (R435-3: the liquidation fill shows fee 0), and the engine
   * takes it on |size| x MARK at the liquidation block (engine.rs liquidate, SPEC 6.3 step 4), not on the fill price.
   * So the check is: the unexplained part of the ledger (a) is exactly what the insurance fund received in the same
   * window (to the unit, plus at most funding rounding residue), and (b) equals ceil(|size| x M x 0.5%) for a mark M
   * that lies inside the band of the index path of this liquidation (0.5% around an index between start and end).
   */
  const ledger = async (victim, o, exitPrice, path, ins0) => {
    const funding = (await api.json(`/v1/accounts/${victim.address}/funding?limit=50`)).payments.filter((p) => p.time >= o.t).reduce((t, p) => t + usdc(p.payment), 0n);
    // realized PnL credited to the balance: floor of the exact amount (S-0.3: the user receives at most the exact value)
    const n = (px(exitPrice) - px(o.entry)) * sz(SIZE), d = 10n ** 10n;
    const realized = n >= 0n ? n / d : -((-n + d - 1n) / d);
    const counterGain = n >= 0n ? -((n + d - 1n) / d) : (-n) / d;   // the other side of the same transfer, also rounded against the user
    const got = await bal(api, victim.address);
    const liqFee = usdc(victim.deposit) - o.openFee + funding + realized - got;
    const dIns = (await bal(api, SYSTEM.insurance)) - ins0;
    expect(liqFee > 0n, `no liquidation fee in the ledger: balance ${fmtUsdc(got)}, funding ${fmtUsdc(funding)}, realized ${fmtUsdc(realized)}`);
    expect(dIns >= liqFee && dIns <= liqFee + 20n, `the victim's ledger is short by ${fmtUsdc(liqFee)}, the insurance fund received ${fmtUsdc(dIns)} (the difference may only be funding rounding residue)`);
    // fee = ceil(size * M * ppm / 1e6 / 1e10)  =>  M in ((fee - 1) * 1e16 / (size * ppm), fee * 1e16 / (size * ppm)]
    const markImplied = Number((liqFee * 10n ** 16n) / (sz(SIZE) * BigInt(LIQ_FEE_PPM))) / 1e8;
    const lo = Math.min(Number(path.from), Number(path.to)) * 0.995, hi = Math.max(Number(path.from), Number(path.to)) * 1.005;
    expect(markImplied >= lo && markImplied <= hi, `the fee ${fmtUsdc(liqFee)} is 0.5% of 0.4 BTC at ${markImplied}, outside the mark band of the path [${lo.toFixed(1)}, ${hi.toFixed(1)}]`);
    expect((await api.account(victim.address)).openOrders === 0, 'orders left');
    return { balance: fmtUsdc(got), deposit: victim.deposit, openFee: fmtUsdc(o.openFee), funding: fmtUsdc(funding), realized: fmtUsdc(realized), liquidationFee: fmtUsdc(liqFee), markImpliedByTheFee: markImplied, insuranceDelta: fmtUsdc(dIns), realizedUnits: realized, counterGainUnits: counterGain };
  };
  const lastFill = async (a) => (await api.json(`/v1/accounts/${a}/fills?limit=1`)).fills[0];
  const lastTradeId = async () => (await api.json('/v1/markets/BTC-USD/trades?limit=1')).trades[0]?.tradeId ?? null;

  // ---------------- E-1x: through the book ----------------
  {
    const v = ctx.who('liq-book');
    let o, ins0, path, bidPx;
    await ctx.check('E-10', 'setup: the victim is long 0.4 BTC at 20x; the maker holds a bid 3.6% below, placed blocks before any liquidation', async () => {
      await api.cancelAll(maker);
      const ref = (await ticker()).indexPrice;
      const ask = onTick(btc, ref, 8); bidPx = onTick(btc, ref, -360);
      let r = await api.order(maker, btc, { buy: false, price: ask, size: '2', tif: 2 });
      expect(r.body?.result?.ok, JSON.stringify(r.body));
      r = await api.order(maker, btc, { buy: true, price: bidPx, size: '1', tif: 2 });
      expect(r.body?.result?.ok, JSON.stringify(r.body));
      o = await open(v, ask);
      const p = await position(v.address);
      expect(Number(p.size) === Number(SIZE) && Number(p.leverage) === 20 && p.liquidationPrice !== null, JSON.stringify(p));
      ins0 = await bal(api, SYSTEM.insurance);
      return { index: ref, entry: ask, makerBid: bidPx, position: p };
    });
    await ctx.check('E-11', 'the index falls 3.8% in 12 rounds: the position is liquidated THROUGH THE BOOK at the resting bid (public trade flagged liquidation; maker pays its maker fee, the victim no taker fee)', async () => {
      expect(o, 'setup failed');
      const tape0 = await lastTradeId();
      path = await ramp('liq-book', onTick(btc, (await ticker()).indexPrice, -380), 12);
      await liquidated(v);
      const f = await lastFill(v.address), fm = await lastFill(maker.address);
      expect(f.liquidation === true && f.side === 'sell' && Number(f.price) === Number(bidPx) && Number(f.size) === Number(SIZE) && f.orderId !== '0', `victim fill ${JSON.stringify(f)}`);
      expect(fm.liquidation === true && fm.liquidity === 'maker' && fm.tradeId === f.tradeId && usdc(fm.fee) === tradeFee(bidPx, SIZE, 150), `maker fill ${JSON.stringify(fm)}`);
      const tr = (await api.json('/v1/markets/BTC-USD/trades?limit=1')).trades[0];
      expect(tr.tradeId !== tape0 && tr.tradeId === f.tradeId && tr.liquidation === true, `tape ${JSON.stringify(tr)}`);
      return { path, victimFill: f, makerFill: fm };
    });
    await ctx.check('E-12', 'the victim\'s ledger closes to the unit: deposit - open fee + funding + realized loss - liquidation fee, and that fee (0.5% of size x mark) is exactly what the insurance fund received', async () => {
      const f = await lastFill(v.address);
      const { realizedUnits, counterGainUnits, ...ev } = await ledger(v, o, f.price, path, ins0);
      expect(usdc(f.realizedPnl) === realizedUnits, `published realizedPnl ${f.realizedPnl}, exact ${fmtUsdc(realizedUnits)}`);
      return ev;
    });
  }

  // ---------------- E-2x: auto-deleveraging (no bid in reach, backstop without funds) ----------------
  {
    const v = ctx.who('liq-adl');
    let o, ins0, tape0, m0, path;
    await ctx.check('E-20', 'setup: no bid in the book, the backstop has no balance; the victim is long 0.4 BTC at 20x against the maker', async () => {
      await api.cancelAll(maker);
      expect((await position(maker.address)) === null, `maker not flat: ${JSON.stringify(await position(maker.address))}`);
      const b = await api.account(SYSTEM.backstop);
      expect(usdc(b.collateral) === 0n && b.positions.length === 0, `backstop ${JSON.stringify(b).slice(0, 200)}`);
      const ask = onTick(btc, (await ticker()).indexPrice, 8);
      const r = await api.order(maker, btc, { buy: false, price: ask, size: '2', tif: 2 });
      expect(r.body?.result?.ok, JSON.stringify(r.body));
      o = await open(v, ask);
      ins0 = await bal(api, SYSTEM.insurance); tape0 = await lastTradeId(); m0 = await bal(api, maker.address);
      return { entry: ask, makerShort: (await position(maker.address)).size };
    });
    await ctx.check('E-21', 'the index falls again: with no book and no backstop the position is closed by ADL against the profitable short, outside the book, without fee to either side and never beyond the victim\'s bankruptcy price', async () => {
      expect(o, 'setup failed');
      path = await ramp('liq-adl', onTick(btc, (await ticker()).indexPrice, -400), 12);
      await liquidated(v);
      const f = await lastFill(v.address), fm = await lastFill(maker.address);
      expect(f.liquidation === true && f.orderId === '0' && f.side === 'sell' && Number(f.size) === Number(SIZE), `victim fill ${JSON.stringify(f)}`);
      expect(fm.liquidation === true && fm.orderId === '0' && fm.side === 'buy' && fm.price === f.price && Number(fm.size) === Number(SIZE) && usdc(fm.fee) === 0n, `counterparty fill ${JSON.stringify(fm)}`);
      expect((await lastTradeId()) === tape0, 'an ADL transfer appeared on the public tape');
      expect((await position(maker.address)) === null, 'the ADL counterparty is not flat (I-1)');
      const bankruptcy = Number(o.entry) - Number(fmtUsdc(usdc(v.deposit) - o.openFee)) / Number(SIZE);
      expect(Number(f.price) >= bankruptcy && Number(f.price) < Number(o.entry), `ADL price ${f.price} against bankruptcy ${bankruptcy.toFixed(2)}`);
      const b = await api.account(SYSTEM.backstop);
      expect(b.positions.length === 0 && usdc(b.collateral) === 0n, 'the unfunded backstop took something');
      return { path, victimFill: f, counterpartyFill: fm, bankruptcyPriceAtOpen: bankruptcy.toFixed(2) };
    });
    await ctx.check('E-22', 'the victim\'s ledger closes to the unit (fee = what the insurance fund received); the counterparty realized exactly the victim\'s loss at the ADL price', async () => {
      const f = await lastFill(v.address);
      const { realizedUnits, counterGainUnits, ...ev } = await ledger(v, o, f.price, path, ins0);
      const mFund = (await api.json(`/v1/accounts/${maker.address}/funding?limit=50`)).payments.filter((p) => p.time >= o.t).reduce((t, p) => t + usdc(p.payment), 0n);
      // the maker sold at the entry and bought back at the ADL price: its gain is the victim's loss, each side rounded
      // against the user (the loss up, the gain down: S-0.3); the at most one unit between them goes to the dust
      const mWant = m0 + mFund + counterGainUnits;
      expect((await bal(api, maker.address)) === mWant, `maker ${fmtUsdc(await bal(api, maker.address))}, expected ${fmtUsdc(mWant)}`);
      expect(-realizedUnits - counterGainUnits >= 0n && -realizedUnits - counterGainUnits <= 1n, 'loss and gain differ by more than the rounding unit');
      return { ...ev, counterpartyGain: fmtUsdc(counterGainUnits), roundingResidueUnits: String(-realizedUnits - counterGainUnits) };
    });
  }

  // ---------------- E-3x: through the backstop, funded through the vault ----------------
  {
    const v = ctx.who('liq-backstop');
    let o, ins0, tape0, path;
    await ctx.check('E-30', 'the insurance fund (60,000) and the backstop (25,000) are seeded by plain deposits through the vault to the system accounts', async () => {
      blockedIf(!chain, 'fake L2 stand-in: no vault to deposit through');
      const funder = user('treasury').address;
      const [i0, b0] = [await bal(api, SYSTEM.insurance), await bal(api, SYSTEM.backstop)];
      let last;
      for (const [to, amt] of [[SYSTEM.insurance, '60000'], [SYSTEM.backstop, '25000']]) {
        const units = String(usdc(amt));
        let r = await chain.send(funder, stack.token, 'mint(address,uint256)', [funder, units]); expect(r.ok, `mint: ${r.error}`);
        r = await chain.send(funder, stack.token, 'approve(address,uint256)', [stack.vault, units]); expect(r.ok, `approve: ${r.error}`);
        last = await chain.send(funder, stack.vault, 'deposit(uint64,address)', [units, to]);
        blockedIf(!last.ok && /InvalidDeposit/.test(last.error ?? ''), 'this tree\'s vault refuses deposits to system accounts (vault v0.2): the backstop cannot be funded');
        expect(last.ok, `deposit to ${to}: ${last.error}`);
      }
      await chain.waitFinalized(last.block);
      await api.until(async () => (await bal(api, SYSTEM.backstop)) === b0 + usdc('25000'), { ms: 60000, what: 'backstop credit' });
      expect((await bal(api, SYSTEM.insurance)) - i0 >= usdc('60000'), 'insurance not credited');
      return { insurance: fmtUsdc(await bal(api, SYSTEM.insurance)), backstop: fmtUsdc(await bal(api, SYSTEM.backstop)) };
    });
    await ctx.check('E-31', 'third fall, again no bid in reach: the BACKSTOP takes the whole position at the bound price; the short maker is not deleveraged; nothing on the public tape', async () => {
      expect(usdc((await api.account(SYSTEM.backstop)).collateral) > 0n, 'the backstop has no balance (E-30)');
      await api.cancelAll(maker);
      const ask = onTick(btc, (await ticker()).indexPrice, 8);
      const r = await api.order(maker, btc, { buy: false, price: ask, size: '2', tif: 2 });
      expect(r.body?.result?.ok, JSON.stringify(r.body));
      o = await open(v, ask);
      ins0 = await bal(api, SYSTEM.insurance); tape0 = await lastTradeId();
      path = await ramp('liq-backstop', onTick(btc, (await ticker()).indexPrice, -400), 12);
      await liquidated(v);
      const f = await lastFill(v.address);
      expect(f.liquidation === true && f.orderId === '0' && f.side === 'sell' && Number(f.size) === Number(SIZE), `victim fill ${JSON.stringify(f)}`);
      const bp = (await api.account(SYSTEM.backstop)).positions.find((p) => p.symbol === 'BTC-USD');
      expect(bp && Number(bp.size) === Number(SIZE) && Number(bp.entryPrice) === Number(f.price), `backstop position ${JSON.stringify(bp)}`);
      const mp = await position(maker.address);
      expect(mp && Number(mp.size) === -Number(SIZE), `maker position ${JSON.stringify(mp)}`);
      expect((await lastTradeId()) === tape0, 'a backstop take-over appeared on the public tape');
      await api.cancelAll(maker);
      return { path, entry: o.entry, victimFill: f, backstopPosition: bp, makerPosition: mp.size };
    });
    await ctx.check('E-32', 'the victim\'s ledger closes to the unit (fee = what the insurance fund received); sizes still sum to zero (maker short, backstop long)', async () => {
      const f = await lastFill(v.address);
      const { realizedUnits, counterGainUnits, ...ev } = await ledger(v, o, f.price, path, ins0);
      let sum = 0n;
      for (const a of [...ctx.CAST.map((c) => c.address), ...Object.values(SYSTEM)]) { const p = await position(a); if (p) sum += sz(p.size.replace('-', '')) * (p.size.startsWith('-') ? -1n : 1n); }
      expect(sum === 0n, `BTC sizes sum to ${sum}`);
      return ev;
    });
  }
}
