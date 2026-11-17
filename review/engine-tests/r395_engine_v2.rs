//! Agent 395 demonstrations against engine `5a50d58`, v2: SAME scenarios as r395_engine.rs (which passed on `647a401`, i.e. failed-first), with the
//! assertions turned into the FIXED property. Copy into `engine/tests/`, run `cargo test --test r395_engine_v2 -- --nocapture`.
//! THROWAWAY keys only (testkit). UNAUDITED review tooling.
use k256::ecdsa::SigningKey;
use postern_perps_engine::engine::{Event, FillKind, ACCT_INSURANCE};
use postern_perps_engine::hash::Addr;
use postern_perps_engine::num::N_PER_USDC;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;
use postern_perps_engine::engine::system_address;

const T0: u64 = 1_794_880_000_000;
const BTC: u32 = 1;
const ETH: u32 = 2;
const P_BTC: u64 = 6_725_000_000_000; // 67,250.00
const P_ETH: u64 = 348_000_000_000; // 3,480.00
const TICK: u64 = 10_000_000;

fn usdc(x: u64) -> u64 { x * 1_000_000 }

struct Rig { e: Engine, ok: Vec<SigningKey>, h: u64, t: u64, dep: u64, nonce: u64 }

impl Rig {
    fn new() -> Rig {
        Rig { e: Engine::new(genesis(7, vec![market(BTC, "BTC-USD"), market(ETH, "ETH-USD")])), ok: oracle_keys(7), h: 0, t: T0, dep: 0, nonce: T0 }
    }
    fn n(&mut self) -> u64 { self.nonce = self.nonce.max(self.t) + 1; self.nonce }
    fn block(&mut self, dt: u64, prices: &[(u32, u64)], rest: Vec<Input>) -> BlockOutcome {
        self.h += 1;
        self.t += dt;
        let mut inputs = Vec::new();
        if !prices.is_empty() { inputs.push(oracle_round(&self.e, &self.ok, self.t, prices)); }
        inputs.extend(rest);
        self.e.apply_block(&Block { height: self.h, time: self.t, inputs }).expect("block applies")
    }
    fn dep(&mut self, a: Addr, amount: u64) -> Input {
        let i = Input::Deposit { index: self.dep, account: a, amount };
        self.dep += 1;
        i
    }
    fn act(&mut self, k: &SigningKey, a: Action) -> Input { sign_action(&self.e, k, a) }
    fn idx(&self, a: &Addr) -> u32 { self.e.by_addr[a] }
    fn custody(&self) -> i128 { self.e.total_deposits - self.e.total_withdrawn }
    fn escape_sum(&self) -> i128 { (0..self.e.accounts.len() as u32).map(|i| self.e.escape_value(i) as i128).sum() }
}

/// R395-5 + R395-23. B is short BTC and long a little ETH. Oracle rounds stop carrying ETH (a sequencer only has to
/// leave ETH out), ETH halts (H1), BTC rises in in-band steps. B is never liquidated (S-4.10 skip), its equity goes
/// below zero, the sum of escape values exceeds custody (PerpsVault `_checkSolvency` would refuse every checkpoint),
/// and B's forced Withdraw and forced CloseAll are CONSUMED with an error (vault sees them processed: no escape).
#[test]
fn r395_5_and_23_halt_blocks_exit_and_breaks_escape_solvency() {
    let mut r = Rig::new();
    let (ka, kb) = (key(11), key(12));
    let (a, b) = (addr(&ka), addr(&kb));
    let d1 = r.dep(a, usdc(100_000));
    let d2 = r.dep(b, usdc(10_000));
    r.block(3000, &[(BTC, P_BTC), (ETH, P_ETH)], vec![d1, d2]);
    // B sells 1 BTC to A; B buys 0.1 ETH from A.
    let (n1, n2, n3, n4) = (r.n(), r.n(), r.n(), r.n());
    let i1 = r.act(&ka, place(a, n1, BTC, true, ORDER_LIMIT, TIF_GTC, P_BTC, 100_000_000, false));
    let i2 = r.act(&kb, place(b, n2, BTC, false, ORDER_MARKET, TIF_IOC, P_BTC - 100 * TICK, 100_000_000, false));
    let i3 = r.act(&ka, place(a, n3, ETH, false, ORDER_LIMIT, TIF_GTC, P_ETH, 10_000_000, false));
    let i4 = r.act(&kb, place(b, n4, ETH, true, ORDER_MARKET, TIF_IOC, P_ETH + 100 * TICK, 10_000_000, false));
    let out = r.block(3000, &[(BTC, P_BTC), (ETH, P_ETH)], vec![i1, i2, i3, i4]);
    assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
    let bi = r.idx(&b);
    assert_eq!(r.e.accounts[bi as usize].positions[&BTC].size, -100_000_000);
    // Oracle rounds with BTC only: ETH goes stale and halts after 10 s; BTC climbs 4.5% per round.
    let mut p = P_BTC;
    let mut liquidated = false;
    // first let ETH go stale (> maxStalenessMs = 10 s) with BTC unchanged, then move BTC
    for _ in 0..5 {
        r.block(3000, &[(BTC, p)], vec![]);
    }
    assert!(!r.e.markets[&ETH].oracle.status.eq(&postern_perps_engine::oracle::Status::Active), "ETH halted before the move");
    for _ in 0..8 {
        p = p / 10_000 * 10_450 / TICK * TICK;
        let out = r.block(3000, &[(BTC, p)], vec![]);
        liquidated |= out.events.iter().any(|e| matches!(e, Event::Liquidated { account } if *account == bi));
    }
    assert!(!r.e.markets[&ETH].oracle.status.eq(&postern_perps_engine::oracle::Status::Active), "ETH halted");
    let eq_b = r.e.equity(bi);
    println!("B equity (USDC) {:.2}, liquidated: {liquidated}", eq_b as f64 / 1e16);
    let (sum, cust) = (r.escape_sum(), r.custody());
    let ins = r.e.accounts[ACCT_INSURANCE as usize].balance;
    println!("FIXED? sum(escapeValue) {} vs custody {} (insurance balance {})", sum, cust, ins);
    assert!(sum <= cust || r.e.escape_value(ACCT_INSURANCE) == 0, "R395-23: deficit charged to INSURANCE first; any excess only beyond its balance");
    let before = r.e.forced_processed;
    let f1 = Input::Forced { index: r.e.next_forced, account: b, kind: FORCED_WITHDRAW, market_id: 0, amount: 0, deadline: 0 };
    let out = r.block(3000, &[(BTC, p)], vec![f1]);
    let ev: Vec<_> = out.events.iter().filter(|e| matches!(e, Event::ForcedPending { .. } | Event::ForcedServed { .. } | Event::ForcedRejected { .. })).collect();
    println!("forced withdraw during the halt: {ev:?}; processed {} -> {}", before, r.e.forced_processed);
    assert!(!out.events.iter().any(|e| matches!(e, Event::ForcedRejected { .. })), "R395-5: never rejected (consumed) for a halt");
    let served = out.events.iter().any(|e| matches!(e, Event::ForcedServed { .. }));
    assert!(served || r.e.forced_processed == before, "R395-5: either served or left pending (vault deadline then opens escape)");
    let f2 = Input::Forced { index: r.e.next_forced, account: b, kind: FORCED_CLOSE, market_id: 0, amount: 0, deadline: 0 };
    let out = r.block(3000, &[(BTC, p)], vec![f2]);
    let ev: Vec<_> = out.events.iter().filter(|e| matches!(e, Event::ForcedPending { .. } | Event::ForcedServed { .. } | Event::ForcedRejected { .. })).collect();
    println!("forced close during the halt: {ev:?}");
    assert!(!out.events.iter().any(|e| matches!(e, Event::ForcedRejected { .. })));
}

/// R395-13. B (short 1 BTC, 10x) falls below maintenance but stays above bankruptcy. In the SAME block as the price
/// move, C rests an ask at the bankruptcy price (rounded down to the tick). Liquidation (step 9, after actions) buys
/// from C at that price: B's remaining equity goes to C, the insurance fund receives (almost) nothing.
#[test]
fn r395_13_liquidation_at_bankruptcy_price_gives_margin_to_same_block_maker() {
    let mut r = Rig::new();
    let (ka, kb, kc) = (key(21), key(22), key(23));
    let (a, b, c) = (addr(&ka), addr(&kb), addr(&kc));
    let d1 = r.dep(a, usdc(100_000));
    let d2 = r.dep(b, usdc(7_000));
    let d3 = r.dep(c, usdc(20_000));
    r.block(3000, &[(BTC, P_BTC)], vec![d1, d2, d3]);
    let (n1, n2) = (r.n(), r.n());
    let i1 = r.act(&ka, place(a, n1, BTC, true, ORDER_LIMIT, TIF_GTC, P_BTC, 100_000_000, false));
    let i2 = r.act(&kb, place(b, n2, BTC, false, ORDER_MARKET, TIF_IOC, P_BTC - 100 * TICK, 100_000_000, false));
    let out = r.block(3000, &[(BTC, P_BTC)], vec![i1, i2]);
    assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
    let bi = r.idx(&b);
    // first step +4.8%: still healthy
    let p1 = P_BTC / 1000 * 1048 / TICK * TICK;
    r.block(3000, &[(BTC, p1)], vec![]);
    // bankruptcy price of B's short (E = 0): (balance*1e10 - openNotional) / |size|
    let acc = &r.e.accounts[bi as usize];
    let pos = &acc.positions[&BTC];
    let bp = (acc.balance * N_PER_USDC - pos.open_notional) / (-(pos.size as i128));
    let ask = (bp as u64) / TICK * TICK;
    let p2 = p1 / 1000 * 1046 / TICK * TICK; // second step: E between 0 and maintenance
    let ins_before = r.e.accounts[ACCT_INSURANCE as usize].balance;
    let n3 = r.n();
    let sniper = r.act(&kc, place(c, n3, BTC, false, ORDER_LIMIT, TIF_GTC, ask, 100_000_000, false));
    let out = r.block(3000, &[(BTC, p2)], vec![sniper]);
    assert_eq!(out.outcomes[1], Ok(()), "C's ask rests");
    let liq_fill = out.events.iter().find_map(|e| match e {
        Event::Fill { kind: FillKind::Liquidation, maker, price, size, .. } => Some((*maker, *price, *size)),
        _ => None,
    });
    let ci = r.idx(&c);
    println!("index {} bp {} ask {} fill {:?}", p2, bp, ask, liq_fill);
    if let Some((maker, price, _)) = liq_fill { assert!(!(maker == ci && price == ask), "R395-13: no fill at the bankruptcy-priced ask"); }
    let ins_gain = r.e.accounts[ACCT_INSURANCE as usize].balance - ins_before;
    let c_upnl = r.e.equity(ci) - r.e.accounts[ci as usize].balance * N_PER_USDC;
    println!("insurance gained {} USDC units; C unrealised at mark {:.2} USDC; B equity left {:.2} USDC",
        ins_gain, c_upnl as f64 / 1e16, r.e.equity(bi) as f64 / 1e16);
    // FIXED property: the liquidation price is bounded at mark -/+ mmf0/2, so C's same-block ask at bankruptcy
    // either does not fill, or fills only inside the bound: C gains at most mmf0/2 of the notional (~0.5% / 2).
    let notional_usd = (p2 as i128) / 100_000_000; // 1 BTC
    assert!(c_upnl <= notional_usd * 10_000_000_000_000_000i128 * 6 / 1000, "R395-13: C's gain bounded by ~mmf0/2");
}

/// R395-31. The insurance fund holds 90% of a gap deficit. SPEC 6.3 step 5: the deficit is paid by INSURANCE and only
/// what it cannot cover goes to ADL. engine.rs liquidate(): when insurance < deficit it pays NOTHING and the whole
/// position is auto-deleveraged at the bankruptcy price against the profitable counterparty.
#[test]
fn r395_31_partial_insurance_unused_whole_gap_socialised() {
    let mut r = Rig::new();
    let (ka, kb) = (key(31), key(32));
    let (a, b) = (addr(&ka), addr(&kb));
    let d1 = r.dep(a, usdc(100_000));
    let d2 = r.dep(b, usdc(2_000));
    r.block(3000, &[(BTC, P_BTC)], vec![d1, d2]);
    let nl = r.n();
    let lev = r.act(&kb, Action::UpdateLeverage { account: b, nonce: nl, market_id: BTC, leverage: 50, isolated: false });
    let (n1, n2) = (r.n(), r.n());
    let i1 = r.act(&ka, place(a, n1, BTC, true, ORDER_LIMIT, TIF_GTC, P_BTC, 100_000_000, false));
    let i2 = r.act(&kb, place(b, n2, BTC, false, ORDER_MARKET, TIF_IOC, P_BTC - 100 * TICK, 100_000_000, false));
    let out = r.block(3000, &[(BTC, P_BTC)], vec![lev, i1, i2]);
    assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
    let bi = r.idx(&b);
    // deficit at +4.9%: E = balance - 0.049 * 67250 ~ 2000 - 15 - 3295 ~ -1310 USDC. Fund insurance with ~90%.
    let p1 = P_BTC / 1000 * 1049 / TICK * TICK;
    let acc = &r.e.accounts[bi as usize];
    let e_at_p1 = acc.balance * N_PER_USDC + (acc.positions[&BTC].size as i128) * (p1 as i128) - acc.positions[&BTC].open_notional;
    let deficit = (-e_at_p1) / N_PER_USDC;
    assert!(deficit > 0);
    let fund = (deficit * 9 / 10) as u64;
    let di = r.dep(system_address(ACCT_INSURANCE), fund);
    r.block(1000, &[], vec![di]);
    let ins_before = r.e.accounts[ACCT_INSURANCE as usize].balance;
    assert_eq!(ins_before as u64, fund);
    let out = r.block(3000, &[(BTC, p1)], vec![]);
    let adl = out.events.iter().any(|e| matches!(e, Event::Fill { kind: FillKind::Adl, .. }));
    let covered = out.events.iter().any(|e| matches!(e, Event::InsuranceCover { account, .. } if *account == bi));
    let ins_after = r.e.accounts[ACCT_INSURANCE as usize].balance;
    println!("deficit {} USDC units, insurance {} -> {}, ADL {adl}, insurance cover {covered}", deficit, ins_before, ins_after);
    let _ = adl;
    assert!(covered && ins_after < ins_before, "R395-31: insurance pays first, as far as it goes");
}
