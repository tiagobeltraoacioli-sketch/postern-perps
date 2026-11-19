//! Round 2 (agent 430), SPEC 6.5 (0.2.6): failing-first for the redesign of the backstop's loss cap. Uses only the
//! API of 2843285 (the first form of the cap, SPEC 0.2.5) and FAILS there; passes on the redesigned engine.
mod common;
use common::*;
use postern_perps_engine::engine::{system_address, Event, ACCT_BACKSTOP};
use postern_perps_engine::genesis::Genesis;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

fn budget_genesis(seed_units: u64, budget_ppm: Option<u32>) -> Genesis {
    let mut g = genesis_raw(7, vec![market(1, "BTC-USD")]);
    g.launch_backstop_min = seed_units;
    g.backstop_loss_budget_ppm = budget_ppm;
    g
}

fn liquidate_a(r: &mut Rig, p: &mut u64) {
    let (ka, kb) = (key(1), key(2));
    r.trade(&[(1, *p)], 1, &kb, &ka, *p, 10_000_000);
    for _ in 0..12 {
        *p = tick(*p * 985 / 1000);
        let out = r.block(3000, &[(1, *p)], vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })) {
            return;
        }
    }
    panic!("a was not liquidated");
}

/// Adversarial (the coordinator's question on 2843285, R436-1 class). Two colluding accounts push the MARK 0.5%
/// under a constant index (a trade between them and a resting ask at the bottom of the mark band), while one of
/// them rests a bid at that pushed mark. On 2843285 the stop was triggered by the mark and executed "at the mark
/// or better": the backstop's inventory was sold into the colluder's bid 0.5% under the index, a transfer from the
/// backstop to the pair. Fixed: trigger and price are the INDEX; with the index constant the backstop is not
/// stopped and sells nothing, whatever the book shows.
#[test]
fn mark_manipulation_neither_triggers_the_stop_nor_buys_the_inventory_cheap() {
    // floor chosen so that the backstop sits just above its stop level after the takeover: seed 1,000, budget 0.1%
    let mut r = Rig::new(budget_genesis(1_000_000_000, Some(1_000)));
    let (ka, kb, kx, ky) = (key(1), key(2), key(5), key(6));
    let (a, b, x, y) = (addr(&ka), addr(&kb), addr(&kx), addr(&ky));
    let deps = vec![r.dep(a, 700), r.dep(b, 80_000), r.dep(x, 50_000), r.dep(y, 50_000), r.dep(system_address(ACCT_BACKSTOP), 1_100)];
    r.block(3000, &[(1, P)], deps);
    let mut p = P;
    liquidate_a(&mut r, &mut p);
    let took = r.size(ACCT_BACKSTOP, 1);
    assert!(took > 0);
    let bal0 = r.e.accounts[ACCT_BACKSTOP as usize].balance;
    // the index falls in small steps until the backstop is within 0.4% of its inventory's notional above its stop
    // level: a 0.5% push of the mark would cross it if the mark counted
    let mut near = false;
    for _ in 0..400 {
        let margin = r.e.equity(ACCT_BACKSTOP) - r.e.backstop_stop_level(); // empty book: mark = index
        let n = took as i128 * p as i128;
        if r.size(ACCT_BACKSTOP, 1) == 0 {
            break;
        }
        if margin < n * 4 / 1000 {
            near = true;
            break;
        }
        p = tick(p * 9995 / 10000);
        r.block(3000, &[(1, p)], vec![]);
    }
    assert!(near, "the backstop is close to its stop level and still holds its inventory");
    // the pair: x sells to y at the bottom of the mark band, y leaves a bid there, x leaves an ask there
    let low = tick(p - p / 200) + TICK;
    let n0 = r.t + 3000;
    let inputs = vec![
        sign_action(&r.e, &ky, place(y, n0, 1, true, ORDER_LIMIT, TIF_GTC, low, 2_000_000, false)),
        sign_action(&r.e, &kx, place(x, n0, 1, false, ORDER_LIMIT, TIF_IOC, low, 1_000_000, false)),
        sign_action(&r.e, &kx, place(x, n0 + 1, 1, false, ORDER_LIMIT, TIF_GTC, low + TICK, 1_000_000, false)),
        sign_action(&r.e, &ky, place(y, n0 + 2, 1, true, ORDER_LIMIT, TIF_GTC, low, 50_000_000, false)),
    ];
    let out = r.block(3000, &[(1, p)], inputs);
    assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
    for _ in 0..3 {
        let out = r.block(3000, &[(1, p)], vec![]);
        assert!(r.e.markets[&1].mark < p - p / 250, "the mark is pushed about 0.5% under the constant index: {} vs {p}", r.e.markets[&1].mark);
        assert!(!out.events.iter().any(|e| matches!(e, Event::Fill { taker: ACCT_BACKSTOP, .. })), "the backstop sold into the pushed book: {:?}", out.events);
    }
    assert_eq!(r.size(ACCT_BACKSTOP, 1), took, "inventory untouched while the index is constant");
    assert_eq!(r.e.accounts[ACCT_BACKSTOP as usize].balance, bal0, "and nothing was realised");
    let _ = (a, b);
}
