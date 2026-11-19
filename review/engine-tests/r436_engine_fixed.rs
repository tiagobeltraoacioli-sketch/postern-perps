//! Agent 436: FIXED-PROPERTY ports of my engine demonstrations, for engine `c6d4ff9` (430's fixes). Same scenarios as
//! `r436_engine.rs` (which passed on `f0e3967` = defect present and FAIL on `c6d4ff9`), assertions turned around.
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


/// R436-1 fixed. (a) The half-price ask is refused PRICE_BAND. (b) Inside the band: A (2,000 USDC, 50x) tries to
/// rest an ask for 1 BTC 4.9% below the mark: refused, the fill would not be backed. (c) A rests it AT the mark
/// (backed), the index then rises 4.9% in one round, B lifts the now-stale ask: the maker would end below
/// maintenance, the order is cancelled instead of filled. In no case does INSURANCE pay anything.
#[test]
fn r436_1_fixed_no_maker_fill_can_be_charged_to_the_insurance_fund() {
    let mut r = Rig::new();
    let (ka, kb) = (key(4361), key(4362));
    let (a, b) = (addr(&ka), addr(&kb));
    let d = vec![r.dep(system_address(ACCT_INSURANCE), usdc(60_000)), r.dep(a, usdc(2_000)), r.dep(b, usdc(10_000))];
    all_ok(&r.block(P, d));
    let n = r.n();
    let lev = r.act(&ka, Action::UpdateLeverage { account: a, nonce: n, market_id: BTC, leverage: 50, isolated: false });
    let half = r.order(&ka, false, ORDER_LIMIT, TIF_GTC, P / 2, ONE);
    let near = r.order(&ka, false, ORDER_LIMIT, TIF_GTC, P / 1000 * 951 / TICK * TICK, ONE);
    let fair = r.order(&ka, false, ORDER_LIMIT, TIF_GTC, P, ONE);
    let out = r.block(P, vec![lev, half, near, fair]);
    println!("half price: {:?}; 4.9% below the mark: {:?}; at the mark: {:?}", out.outcomes[2], out.outcomes[3], out.outcomes[4]);
    assert_eq!(out.outcomes[2], Err(Error::PriceBand));
    assert_eq!(out.outcomes[3], Err(Error::InsufficientMargin));
    assert_eq!(out.outcomes[4], Ok(()));
    let up = P / 1000 * 1049 / TICK * TICK;
    let buy = r.order(&kb, true, ORDER_LIMIT, TIF_IOC, P, ONE);
    let out = r.block(up, vec![buy]);
    println!("index +4.9%, B lifts the stale ask: fills {}, events {:?}", out.events.iter().filter(|e| matches!(e, Event::Fill { .. })).count(), out.events.iter().filter(|e| matches!(e, Event::Cancelled { .. } | Event::RemainderCancelled { .. })).collect::<Vec<_>>());
    assert!(!out.events.iter().any(|e| matches!(e, Event::Fill { .. })), "no fill");
    assert!(out.events.iter().any(|e| matches!(e, Event::Cancelled { reason: Error::InsufficientMargin, .. })), "the maker's order is cancelled");
    assert!(!out.events.iter().any(|e| matches!(e, Event::InsuranceCover { .. })));
    assert_eq!(r.ins(), usdc(60_000) as i128, "INSURANCE untouched");
    postern_perps_engine::invariants::check(&r.e).unwrap();
}

/// R436-10 fixed. Same scenario as the demonstration: S's forced close during the stale halt is served, but at the
/// side of the exit band worse for S (a short buys at `high` = frozen index + at least exitHaircutBps 5%), paid to
/// A. S ends WORSE than by waiting for the 4% move, A better: the stale index is no longer a free exit.
#[test]
fn r436_10_fixed_a_forced_close_in_a_halt_costs_the_closer_the_exit_band() {
    let run = |forced: bool| -> (i128, i128, Option<u64>) {
        let mut r = Rig::new();
        let (ka, ks) = (key(4391), key(4392));
        let (a, s) = (addr(&ka), addr(&ks));
        let deps = vec![r.dep(a, usdc(50_000)), r.dep(s, usdc(50_000))];
        all_ok(&r.block(P, deps));
        let i1 = r.order(&ka, true, ORDER_LIMIT, TIF_GTC, P, ONE);
        let i2 = r.order(&ks, false, ORDER_MARKET, TIF_IOC, P - 100 * TICK, ONE);
        all_ok(&r.block(P, vec![i1, i2]));
        for _ in 0..4 {
            r.h += 1;
            r.t += 3000;
            r.e.apply_block(&Block { height: r.h, time: r.t, inputs: vec![] }).unwrap();
        }
        let mut price = None;
        if forced {
            r.h += 1;
            r.t += 3000;
            let f = Input::Forced { index: 0, account: s, kind: FORCED_CLOSE, deadline: r.t / 1000 + 86_400, arg: 0 };
            let out = r.e.apply_block(&Block { height: r.h, time: r.t, inputs: vec![f] }).unwrap();
            assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 0 })));
            price = out.events.iter().find_map(|e| if let Event::Fill { price, .. } = e { Some(*price) } else { None });
        }
        let up = P / 100 * 104;
        for _ in 0..4 {
            r.block(up, vec![]);
        }
        let eq = |x: &Addr| r.e.equity(r.e.by_addr[x]) / 10_000_000_000;
        (eq(&a), eq(&s), price)
    };
    let (a_plain, s_plain, _) = run(false);
    let (a_forced, s_forced, price) = run(true);
    println!("no forced close: A {:.2} S {:.2}; forced close at {:?}: A {:.2} S {:.2}", f(a_plain), f(s_plain), price, f(a_forced), f(s_forced));
    assert!(price.unwrap() >= P / 100 * 105, "closed at least 5% above the frozen index");
    assert!(s_forced < s_plain, "the closer is worse off than by waiting");
    assert!(a_forced >= a_plain, "the deleveraged counterparty is not worse off");
}
