//! Round 2 (agent 430), SPEC 6.5 (0.2.5): the loss cap of the backstop (R436-5's residue). The failing-first
//! demonstration is `r436_5_the_backstop_loss_is_capped_by_a_stop_at_the_mark` in `r2_failing_first.rs`; here: the
//! floor from the genesis loss budget, the order insurance -> backstop -> ADL once the budget is used, what users
//! see, and the escape values with a backstop at its floor.
mod common;
use common::*;
use postern_perps_engine::engine::{system_address, Event, FillKind, ACCT_BACKSTOP, ACCT_INSURANCE};
use postern_perps_engine::genesis::Genesis;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

fn budget_genesis(seed_units: u64, budget_ppm: Option<u32>) -> Genesis {
    let mut g = genesis_raw(7, vec![market(1, "BTC-USD")]);
    g.launch_backstop_min = seed_units;
    g.backstop_loss_budget_ppm = budget_ppm;
    g
}

#[test]
fn floor_is_the_seed_minus_the_budget_rounded_up_and_the_field_is_optional() {
    let g0 = budget_genesis(1_000_000_000, None);
    let enc0 = String::from_utf8(g0.encoding()).unwrap();
    assert!(!enc0.contains("backstopLossBudgetPpm"), "absent field is not encoded: old genesis hashes stand");
    assert_eq!(g0.clone().check().unwrap().backstop_floor, 0, "absent = budget 100%: floor 0 (the maintenance stop alone)");
    let g = budget_genesis(1_000_000_000, Some(400_000));
    let enc = String::from_utf8(g.encoding()).unwrap();
    assert!(enc.ends_with(",\"backstopLossBudgetPpm\":\"400000\"}"), "{enc}");
    let c = Genesis::from_json(&enc).unwrap();
    assert_eq!((c.g.backstop_loss_budget_ppm, c.backstop_floor), (Some(400_000), 600_000_000));
    // rounding: UP (toward stopping earlier): 1,000,001 units * (1 - 0.333333) = 666,667.99...
    assert_eq!(budget_genesis(1_000_001, Some(333_333)).check().unwrap().backstop_floor, 666_668);
    assert_eq!(budget_genesis(1_000_001, Some(0)).check().unwrap().backstop_floor, 1_000_001);
    assert_eq!(budget_genesis(1_000_001, Some(1_000_000)).check().unwrap().backstop_floor, 0);
    assert!(budget_genesis(1_000_001, Some(1_000_001)).check().is_err());
    // not a number in the JSON form: refused (decimal strings only, as every integer of the genesis)
    assert!(Genesis::from_json(&enc.replace("\"400000\"}", "400000}")).is_err());
}

/// Seed 1,000 USDC, loss budget 40%: floor 600. The backstop takes a liquidated long, loses in 1.5% rounds, and is
/// stopped in the first block that puts its equity under 600: event, ADL fills AT THE MARK naming the
/// counterparty, backstop flat with at least floor minus one round's move. After that its budget is used up: the
/// next liquidation skips it (no Backstop fill) and goes to ADL. Escape values still sum to custody.
#[test]
fn budget_used_up_then_liquidations_skip_the_backstop_and_go_to_adl() {
    let mut r = Rig::new(budget_genesis(1_000_000_000, Some(400_000)));
    let (ka, kb, kc) = (key(1), key(2), key(3));
    let (a, b, c) = (addr(&ka), addr(&kb), addr(&kc));
    let deps = vec![r.dep(a, 700), r.dep(b, 80_000), r.dep(system_address(ACCT_BACKSTOP), 1_000), r.dep(system_address(ACCT_INSURANCE), 1)];
    r.block(3000, &[(1, P)], deps);
    r.trade(&[(1, P)], 1, &kb, &ka, P, 10_000_000); // a long 0.1 BTC against b
    let mut p = P;
    let step = |p: u64| tick(p * 985 / 1000);
    let mut took = false;
    for _ in 0..12 {
        p = step(p);
        let out = r.block(3000, &[(1, p)], vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Fill { kind: FillKind::Backstop, .. })) {
            took = true;
            break;
        }
    }
    assert!(took, "the backstop took a's long: its equity after the takeover is above the floor");
    assert!(r.e.equity(ACCT_BACKSTOP) >= 1_000_000_000 * 10_000_000_000);
    let mut stopped = None;
    for _ in 0..12 {
        p = step(p);
        let before = r.e.equity(ACCT_BACKSTOP);
        let out = r.block(3000, &[(1, p)], vec![]);
        if let Some((equity, stop)) = out.events.iter().find_map(|e| match e { Event::BackstopStopped { equity, stop } => Some((*equity, *stop)), _ => None }) {
            assert!(before >= 600_000_000 * 10_000_000_000, "it was at or above the floor one block earlier");
            assert_eq!(stop, 600_000_000, "the stop level is the floor (above its own maintenance requirement here)");
            assert!(equity < 600_000_000);
            let fills: Vec<(FillKind, u32, u64, u64)> = out.events.iter().filter_map(|e| match e { Event::Fill { taker: ACCT_BACKSTOP, kind, maker, price, size, .. } => Some((*kind, *maker, *price, *size)), _ => None }).collect();
            assert_eq!(fills, vec![(FillKind::Adl, r.ai(&b), p, 10_000_000)], "b is deleveraged at the mark and the event names it");
            stopped = Some(equity);
            break;
        }
        assert!(r.e.equity(ACCT_BACKSTOP) >= r.e.backstop_stop_level());
    }
    let equity_at_stop = stopped.expect("the backstop was stopped");
    let one_round = (10_000_000i128 * p as i128 * 15 / 985) / 10_000_000_000 + 1; // 1.5% of the previous price on 0.1 BTC
    assert_eq!(r.size(ACCT_BACKSTOP, 1), 0);
    let bal = r.e.accounts[ACCT_BACKSTOP as usize].balance;
    assert!(bal >= 600_000_000 - one_round && bal < 600_000_000, "floor minus at most one round's move ({one_round}): {bal}");
    assert!(bal >= equity_at_stop, "closing at the mark realised exactly the equity it had (rounded toward the protocol by at most a unit): {bal} vs {equity_at_stop}");
    // escape values with the backstop at its floor: nothing is under water, the total is custody up to rounding
    let esc = r.e.escape_values();
    let total: i128 = esc.iter().map(|x| *x as i128).sum();
    let custody = r.e.total_deposits - r.e.total_withdrawn;
    assert_eq!(esc[ACCT_BACKSTOP as usize] as i128, bal);
    assert!(total <= custody && custody - total <= r.e.accounts.len() as i128 + 1, "sum of escape values {total} vs custody {custody}");
    // the budget is used up: c's liquidation skips the backstop and is deleveraged
    let d = r.dep(c, 700);
    r.block(3000, &[(1, p)], vec![d]);
    r.trade(&[(1, p)], 1, &kb, &kc, p, 10_000_000);
    let ci = r.ai(&c);
    let mut seen = None;
    for _ in 0..12 {
        p = step(p);
        let out = r.block(3000, &[(1, p)], vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { account } if *account == ci)) {
            seen = Some(out);
            break;
        }
    }
    let out = seen.expect("c is liquidated");
    assert!(!out.events.iter().any(|e| matches!(e, Event::Fill { kind: FillKind::Backstop, .. })), "the backstop is skipped: {:?}", out.events);
    assert!(out.events.iter().any(|e| matches!(e, Event::Fill { kind: FillKind::Adl, taker, maker, .. } if *taker == ci && *maker == r.ai(&b))), "c is closed by ADL against b");
    assert_eq!(r.e.accounts[ACCT_BACKSTOP as usize].balance, bal, "and the backstop loses nothing more");
    assert!(r.e.accounts[ACCT_INSURANCE as usize].balance >= 0);
}

/// The stop uses the book first, at the mark or better for the backstop and never an order of the same block; only
/// the rest is deleveraged.
#[test]
fn the_stop_uses_resting_liquidity_at_the_mark_before_adl() {
    let mut r = Rig::new(budget_genesis(1_000_000_000, Some(400_000)));
    let (ka, kb, kd) = (key(1), key(2), key(4));
    let (a, b, d) = (addr(&ka), addr(&kb), addr(&kd));
    let deps = vec![r.dep(a, 700), r.dep(b, 80_000), r.dep(d, 80_000), r.dep(system_address(ACCT_BACKSTOP), 1_000)];
    r.block(3000, &[(1, P)], deps);
    r.trade(&[(1, P)], 1, &kb, &ka, P, 10_000_000);
    let mut p = P;
    for _ in 0..12 {
        p = tick(p * 985 / 1000);
        let out = r.block(3000, &[(1, p)], vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Fill { kind: FillKind::Backstop, .. })) {
            break;
        }
    }
    assert_eq!(r.size(ACCT_BACKSTOP, 1), 10_000_000);
    // d keeps a bid for 0.04 BTC exactly at the NEXT index, placed one block ahead (so it is not a same-block order
    // when the stop runs, and it sits at the mark, not through it)
    let mut fills = vec![];
    for _ in 0..12 {
        let next = tick(p * 985 / 1000);
        let bid = sign_action(&r.e, &kd, place(d, r.t + 3000, 1, true, ORDER_LIMIT, TIF_GTC, next, 4_000_000, false));
        let cancel = sign_action(&r.e, &kd, Action::CancelAll { account: d, nonce: r.t + 2999, market_id: 1 });
        r.block(1000, &[], vec![cancel, bid]);
        p = next;
        let out = r.block(2000, &[(1, p)], vec![]);
        if out.events.iter().any(|e| matches!(e, Event::BackstopStopped { .. })) {
            fills = out.events.iter().filter_map(|e| match e { Event::Fill { taker: ACCT_BACKSTOP, kind, maker, price, size, .. } => Some((*kind, *maker, *price, *size)), _ => None }).collect();
            break;
        }
    }
    assert_eq!(fills, vec![(FillKind::Liquidation, r.ai(&d), p, 4_000_000), (FillKind::Adl, r.ai(&b), p, 6_000_000)], "the resting bid at the mark first, the rest by ADL at the mark");
}
