//! Round 2 (agent 430), SPEC 6.5 (0.2.6): the loss cap of the backstop (R436-5's residue). Failing-first
//! demonstrations: `r436_5_the_backstop_loss_is_capped_by_a_stop_at_the_mark` in `r2_failing_first.rs` (fails on
//! 5106fea) and `tests/r2_backstop_manipulation.rs` (fails on 2843285, the first form of the cap). Here: the floor
//! from the genesis loss budget, the sized takeover, the order insurance -> backstop -> ADL once the budget is
//! used, what users see, and the escape values with a backstop at its floor.
mod common;
use common::*;
use postern_perps_engine::engine::{system_address, Engine, Event, FillKind, ACCT_BACKSTOP, ACCT_INSURANCE};
use postern_perps_engine::genesis::Genesis;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

const N: i128 = 10_000_000_000;

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
    assert_eq!(g0.clone().check().unwrap().backstop_floor, 0, "absent = budget 100%: floor 0");
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
    assert!(Genesis::from_json(&enc.replace("\"400000\"}", "400000}")).is_err(), "decimal strings only");
    // the buffer of the test market: jump 5% + mark band 0.5% + half of mmf 1% + one funding at the 0.05% cap
    assert_eq!(Engine::backstop_buffer_ppm(&market(1, "BTC-USD")), 50_000 + 5_000 + 5_000 + 500);
}

/// Returns the rig after a (long 0.1 BTC, 700 USDC) was liquidated with the book empty, and the liquidation block.
fn liquidate_a(r: &mut Rig, p: &mut u64) -> BlockOutcome {
    let (ka, kb) = (key(1), key(2));
    r.trade(&[(1, *p)], 1, &kb, &ka, *p, 10_000_000);
    for _ in 0..12 {
        *p = tick(*p * 985 / 1000);
        let out = r.block(3000, &[(1, *p)], vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })) {
            return out;
        }
    }
    panic!("a was not liquidated");
}

/// Seed 1,000 USDC, loss budget 40%: floor 600. (1) The takeover is SIZED: of a's 0.1 BTC the backstop takes the
/// largest step multiple that leaves its index-valued equity at or above floor + MR-or-floor rule + twice the
/// buffer; the rest is deleveraged in the same liquidation. (2) It loses in 1.5% rounds and is stopped before its
/// equity reaches the floor: event, ADL fills AT THE INDEX naming the counterparty, and its balance afterwards is
/// AT OR ABOVE the floor. (3) Its budget is used: the next liquidation skips it. (4) Escape values sum to custody.
#[test]
fn sized_takeover_stop_above_the_floor_then_liquidations_skip_the_backstop() {
    let mut r = Rig::new(budget_genesis(1_000_000_000, Some(400_000)));
    let (ka, kb, kc) = (key(1), key(2), key(3));
    let (a, b, c) = (addr(&ka), addr(&kb), addr(&kc));
    let deps = vec![r.dep(a, 700), r.dep(b, 80_000), r.dep(system_address(ACCT_BACKSTOP), 1_000), r.dep(system_address(ACCT_INSURANCE), 1)];
    r.block(3000, &[(1, P)], deps);
    let mut p = P;
    let out = liquidate_a(&mut r, &mut p);
    let step = r.e.markets[&1].p.step as i64;
    let took = r.size(ACCT_BACKSTOP, 1);
    assert!(took > 0 && took < 10_000_000 && took % step == 0, "a part of the 0.1 BTC, a step multiple: {took}");
    let adl: u64 = out.events.iter().filter_map(|e| match e { Event::Fill { kind: FillKind::Adl, taker, size, .. } if *taker == r.ai(&a) => Some(*size), _ => None }).sum();
    assert_eq!(took as u64 + adl, 10_000_000, "the rest of a's position was deleveraged in the same liquidation");
    // the sizing rule, recomputed: equity (at the index) >= max(floor, MR) + 2 x buffer, and one step more would not fit
    let level = |e: &Engine, size: i64| -> i128 {
        let n = size as i128 * e.markets[&1].oracle.index as i128;
        let mr = (n * 10_000 + 999_999) / 1_000_000;
        let buffer = (n * 60_500 + 999_999) / 1_000_000;
        mr.max(600_000_000 * N) + 2 * buffer
    };
    assert!(r.e.backstop_equity() >= level(&r.e, took));
    assert!(r.e.backstop_equity() < level(&r.e, took + step) + step as i128 * p as i128 / 100, "one step more would not have fitted (up to the price bound's 1%)");
    assert_eq!(r.e.backstop_stop_level(), 600_000_000 * N + (took as i128 * p as i128 * 60_500 + 999_999) / 1_000_000);
    // (2) the fall
    let mut stopped = None;
    for _ in 0..40 {
        p = tick(p * 985 / 1000);
        let out = r.block(3000, &[(1, p)], vec![]);
        if let Some((equity, stop)) = out.events.iter().find_map(|e| match e { Event::BackstopStopped { equity, stop } => Some((*equity, *stop)), _ => None }) {
            assert!(equity < stop && equity >= 600_000_000, "stopped under the level and still at or above the floor: {equity} / {stop}");
            let fills: Vec<(FillKind, u32, u64, u64)> = out.events.iter().filter_map(|e| match e { Event::Fill { taker: ACCT_BACKSTOP, kind, maker, price, size, .. } => Some((*kind, *maker, *price, *size)), _ => None }).collect();
            assert_eq!(fills, vec![(FillKind::Adl, r.ai(&b), p, took as u64)], "b is deleveraged at the index and the event names it");
            stopped = Some(equity);
            break;
        }
        assert!(r.e.backstop_equity() >= r.e.backstop_stop_level());
    }
    let equity_at_stop = stopped.expect("the backstop was stopped");
    assert_eq!(r.size(ACCT_BACKSTOP, 1), 0);
    let bal = r.e.accounts[ACCT_BACKSTOP as usize].balance;
    assert!(bal >= 600_000_000, "THE FLOOR HOLDS while the market is active: {bal}");
    assert!(bal >= equity_at_stop && bal <= equity_at_stop + 1, "closing at the index realised exactly the equity it had: {bal} vs {equity_at_stop}");
    // (4) escape values with the backstop at its floor: nothing is under water, the total is custody up to rounding
    let esc = r.e.escape_values();
    let total: i128 = esc.iter().map(|x| *x as i128).sum();
    let custody = r.e.total_deposits - r.e.total_withdrawn;
    assert_eq!(esc[ACCT_BACKSTOP as usize] as i128, bal);
    assert!(total <= custody && custody - total <= r.e.accounts.len() as i128 + 1, "sum of escape values {total} vs custody {custody}");
    // (3) with about 600 to 800 USDC left it can carry almost nothing above the floor: c's liquidation goes to ADL
    let d = r.dep(c, 700);
    r.block(3000, &[(1, p)], vec![d]);
    r.trade(&[(1, p)], 1, &kb, &kc, p, 10_000_000);
    let ci = r.ai(&c);
    let mut seen = None;
    for _ in 0..12 {
        p = tick(p * 985 / 1000);
        let out = r.block(3000, &[(1, p)], vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { account } if *account == ci)) {
            seen = Some(out);
            break;
        }
    }
    let out = seen.expect("c is liquidated");
    let to_backstop: u64 = out.events.iter().filter_map(|e| match e { Event::Fill { kind: FillKind::Backstop, size, .. } => Some(*size), _ => None }).sum();
    let to_adl: u64 = out.events.iter().filter_map(|e| match e { Event::Fill { kind: FillKind::Adl, taker, size, .. } if *taker == ci => Some(*size), _ => None }).sum();
    assert!(to_backstop < took as u64 && to_adl > 0 && to_backstop + to_adl == 10_000_000, "less than before to the backstop ({to_backstop}), the rest to ADL ({to_adl})");
    assert!(r.e.backstop_equity() >= 600_000_000 * N);
}

/// The stop uses the book first, at the index or better for the backstop and never an order of the same block; only
/// the rest is deleveraged.
#[test]
fn the_stop_uses_resting_liquidity_at_the_index_before_adl() {
    let mut r = Rig::new(budget_genesis(1_000_000_000, Some(400_000)));
    let (ka, kb, kd) = (key(1), key(2), key(4));
    let (a, b, d) = (addr(&ka), addr(&kb), addr(&kd));
    let deps = vec![r.dep(a, 700), r.dep(b, 80_000), r.dep(d, 80_000), r.dep(system_address(ACCT_BACKSTOP), 1_000)];
    r.block(3000, &[(1, P)], deps);
    let mut p = P;
    liquidate_a(&mut r, &mut p);
    let took = r.size(ACCT_BACKSTOP, 1) as u64;
    assert!(took > 1_000_000);
    // d keeps a bid for 0.01 BTC exactly at the NEXT index, placed one block ahead (so it is not a same-block order
    // when the stop runs, and it sits at the index, not through it)
    let mut fills = vec![];
    for _ in 0..40 {
        let next = tick(p * 985 / 1000);
        let cancel = sign_action(&r.e, &kd, Action::CancelAll { account: d, nonce: r.t + 999, market_id: 1 });
        let bid = sign_action(&r.e, &kd, place(d, r.t + 1000, 1, true, ORDER_LIMIT, TIF_GTC, next, 1_000_000, false));
        r.block(1000, &[], vec![cancel, bid]);
        p = next;
        let out = r.block(2000, &[(1, p)], vec![]);
        if out.events.iter().any(|e| matches!(e, Event::BackstopStopped { .. })) {
            fills = out.events.iter().filter_map(|e| match e { Event::Fill { taker: ACCT_BACKSTOP, kind, maker, price, size, .. } => Some((*kind, *maker, *price, *size)), _ => None }).collect();
            break;
        }
    }
    assert_eq!(fills, vec![(FillKind::Liquidation, r.ai(&d), p, 1_000_000), (FillKind::Adl, r.ai(&b), p, took - 1_000_000)], "the resting bid at the index first, the rest by ADL at the index");
}
