//! Round 2 (agent 430): the open findings of review 395 that needed a stated rule and a pin (SPEC 0.2):
//! R395-15 (imf rounding), R395-17 (ADL rank for E <= 0, bankruptcy price recomputed per position, INSURANCE never
//! below zero), R395-20 (nonce sets outlive agent removal), R395-21 (deposit to an agent address), R395-22 (exact
//! refusals and the genesis `collateralToken`). The failing-first demonstrations are in `r2_failing_first.rs`.
mod common;
use common::*;
use postern_perps_engine::engine::{adl_score, system_address, Event, FillKind, ACCT_BACKSTOP, ACCT_INSURANCE, ACCT_PROTOCOL_FEE};
use postern_perps_engine::genesis::Genesis;
use postern_perps_engine::hash::{hex0x, parse_addr};
use postern_perps_engine::num::{div_ceil, div_floor, N_PER_USDC};
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

/// R395-15 (closed in code since 647a401, pinned here): `imf' = max(imf, ceil(10^6 / leverage))`. Leverage 3 gives
/// 333334 ppm, not 333333: on a 20,175 USD bid the requirement is 6,725.013450 USDC; the floor would give
/// 6,724.993275 (20,175 micro-USDC less margin than the exact third: toward the user, against S-0.3).
#[test]
fn r395_15_leverage_fraction_rounds_up() {
    let mut r = Rig::one();
    let ka = key(1);
    let a = addr(&ka);
    let d = r.dep(a, 10_000);
    r.block(3000, &[(1, P)], vec![d]);
    let lev = sign_action(&r.e, &ka, Action::UpdateLeverage { account: a, nonce: r.t + 3000, market_id: 1, leverage: 3, isolated: false });
    let bid = sign_action(&r.e, &ka, place(a, r.t + 3001, 1, true, ORDER_LIMIT, TIF_GTC, P - 100 * TICK, 30_000_000, false));
    let out = r.block(3000, &[(1, P)], vec![lev, bid]);
    assert_eq!(out.outcomes[1..], [Ok(()), Ok(())]);
    let n: i128 = 30_000_000 * P as i128; // |bids| * max(mark, bid) in notional units
    let ir_up = div_ceil(div_ceil(n * 333_334, 1_000_000), N_PER_USDC);
    let ir_down = div_ceil(div_ceil(n * 333_333, 1_000_000), N_PER_USDC);
    assert_eq!((ir_up, ir_down), (6_725_013_450, 6_724_993_275));
    assert_eq!(r.e.free_collateral(r.ai(&a)), 10_000_000_000 - ir_up);
}

/// R395-17 (a), the rule itself.
#[test]
fn r395_17_adl_score_rule() {
    // E > 0: U * 10^6 / E toward zero
    assert_eq!(adl_score(7, 500, 1000), 500_000);
    assert_eq!(adl_score(7, -1, 3), -333_333);
    // E <= 0: last, whatever U
    assert_eq!(adl_score(7, 10_i128.pow(30), 0), i128::MIN);
    assert_eq!(adl_score(7, 10_i128.pow(30), -5), i128::MIN);
    assert!(adl_score(7, -10_i128.pow(30), 1) > adl_score(8, 10_i128.pow(30), -1), "any positive equity ranks before any non-positive one");
    // the backstop first (R395-14), even under water
    assert_eq!(adl_score(ACCT_BACKSTOP, -5, -5), i128::MAX);
    // bounds of SPEC section 1: |U| <= 2 * 10^30 notional units; the product fits
    assert_eq!(adl_score(7, 2 * 10_i128.pow(30), 1), 2 * 10_i128.pow(36));
}

fn gap_market(id: u32, s: &str) -> postern_perps_engine::genesis::MarketParams {
    let mut m = market(id, s);
    m.oracle.jump_bps = 3000; // a 15% move in one round is accepted (a gap past bankruptcy)
    m.oracle.exit_haircut_bps = 3000;
    m
}

/// R395-17 (a) in the engine: a (index 3) is liquidated in a gap with no insurance: ADL at its bankruptcy price.
/// Opposite side: c (index 4), whose total equity is NEGATIVE (a large ETH loss), and d (index 5) with positive
/// equity. d is deleveraged first although c has the lower index; c takes the rest.
#[test]
fn r395_17_adl_ranks_non_positive_equity_last() {
    let mut r = Rig::new(genesis_raw(7, vec![gap_market(1, "BTC-USD"), gap_market(2, "ETH-USD")]));
    let (ka, kc, kd, ke) = (key(1), key(2), key(3), key(4));
    let (a, c, d, e) = (addr(&ka), addr(&kc), addr(&kd), addr(&ke));
    let deps = vec![r.dep(a, 700), r.dep(c, 4_000), r.dep(d, 50_000), r.dep(e, 50_000)];
    let px = [(1, P), (2, PE)];
    r.block(3000, &px, deps);
    assert_eq!((r.ai(&a), r.ai(&c), r.ai(&d)), (3, 4, 5));
    r.trade(&px, 1, &kc, &ka, P, 5_000_000); // c short 0.05 BTC
    r.trade(&px, 1, &kd, &ka, P, 5_000_000); // d short 0.05 BTC, a long 0.1
    r.trade(&px, 2, &ke, &kc, PE, 1_000_000_000); // c long 10 ETH
    let px2 = [(1, tick(P * 85 / 100)), (2, tick(PE * 85 / 100))];
    let out = r.block(3000, &px2, vec![]);
    let adl: Vec<(u32, u32, u64)> = out
        .events
        .iter()
        .filter_map(|ev| match ev {
            Event::Fill { market: 1, kind: FillKind::Adl, taker, maker, size, .. } => Some((*taker, *maker, *size)),
            _ => None,
        })
        .collect();
    assert_eq!(adl, vec![(3, 5, 5_000_000), (3, 4, 5_000_000)], "d (E > 0) first, then c (E <= 0): {:?}", out.events);
    assert_eq!(r.e.accounts[ACCT_INSURANCE as usize].balance.min(0), 0);
}

/// R395-17 (b): the bankruptcy price of the SECOND position is computed after the first has closed (with the
/// balance as it then stands: realised result and liquidation fee of the first included), not from the state at
/// the start of the liquidation. The expected price is rebuilt here from the block's events.
#[test]
fn r395_17_bankruptcy_price_is_recomputed_between_positions() {
    let mut r = Rig::new(two_markets());
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let deps = vec![r.dep(a, 1_400), r.dep(b, 100_000), r.dep(system_address(ACCT_BACKSTOP), 100_000), r.dep(system_address(ACCT_INSURANCE), 1_000)];
    let mut px = [(1, P), (2, PE)];
    r.block(3000, &px, deps);
    r.trade(&px, 1, &kb, &ka, P, 10_000_000); // a long 0.1 BTC
    r.trade(&px, 2, &kb, &ka, PE, 200_000_000); // a long 2 ETH
    let ai = r.ai(&a);
    let mut found = None;
    for _ in 0..12 {
        px = [(1, tick(px[0].1 * 985 / 1000)), (2, tick(px[1].1 * 985 / 1000))];
        let before = r.e.accounts[ai as usize].clone();
        let out = r.block(3000, &px, vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { account } if *account == ai)) {
            found = Some((before, out));
            break;
        }
    }
    let (before, out) = found.expect("a is liquidated");
    let (m1, m2) = (px[0].1 as i128, px[1].1 as i128); // empty books: mark = index
    let fills = |mid: u32| -> Vec<(u64, u64)> {
        out.events.iter().filter_map(|e| match e { Event::Fill { market, taker, price, size, .. } if *market == mid && *taker == ai => Some((*price, *size)), _ => None }).collect()
    };
    let cover: i128 = out.events.iter().filter_map(|e| match e { Event::InsuranceCover { account, amount } if *account == ai => Some(*amount), _ => None }).sum();
    let fees: Vec<i128> = out.events.iter().filter_map(|e| match e { Event::LiquidationFee { account, amount } if *account == ai => Some(*amount), _ => None }).collect();
    let (f1, f2) = (fills(1), fills(2));
    assert_eq!((f1.len(), f2.len()), (1, 1), "one backstop fill per market: {:?}", out.events);
    let (p1, p2) = (&before.positions[&1], &before.positions[&2]);
    // balance after position 1 closed at its fill price (gain floored, S-0.3) and its liquidation fee
    let realised1 = (p1.size as i128) * (f1[0].0 as i128) - p1.open_notional;
    let fee1 = fees.first().copied().unwrap_or(0);
    let bal1 = before.balance + cover + div_floor(realised1, N_PER_USDC) - fee1;
    let bp2 = div_ceil(p2.open_notional - bal1 * N_PER_USDC, p2.size as i128);
    let lp2 = bp2.max(m2 - m2 * 5_000 / 1_000_000);
    assert_eq!(f2[0].0 as i128, lp2, "ETH closes at the bound computed AFTER BTC closed");
    // the test discriminates: the price from the state at the start of the liquidation is another one
    let rest0 = (before.balance + cover) * N_PER_USDC + (p1.size as i128) * m1 - p1.open_notional;
    let bp2_start = div_ceil(p2.open_notional - rest0, p2.size as i128);
    let lp2_start = bp2_start.max(m2 - m2 * 5_000 / 1_000_000);
    assert_ne!(lp2, lp2_start, "recomputation changes the price in this scenario");
    assert!(r.e.accounts[ai as usize].positions.is_empty());
    assert!(r.bal(&a) >= 0, "bankruptcy rounds toward the account: no bad debt from a liquidation that is not a gap");
}

/// R395-17 (c): INSURANCE pays a deficit only as far as its balance goes and never goes below zero; what it
/// cannot pay is deleveraged at the bankruptcy price (no bad debt is left), and I-2 stays exact.
#[test]
fn r395_17_insurance_never_goes_below_zero() {
    let mut r = Rig::new(genesis_raw(7, vec![gap_market(1, "BTC-USD")]));
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let deps = vec![r.dep(a, 700), r.dep(b, 50_000), r.dep(system_address(ACCT_INSURANCE), 10)];
    r.block(3000, &[(1, P)], deps);
    r.trade(&[(1, P)], 1, &kb, &ka, P, 10_000_000);
    let out = r.block(3000, &[(1, tick(P * 80 / 100))], vec![]);
    let cover: i128 = out.events.iter().filter_map(|e| match e { Event::InsuranceCover { amount, .. } => Some(*amount), _ => None }).sum();
    assert_eq!(cover, 10_000_000, "all of it, not more");
    assert_eq!(r.e.accounts[ACCT_INSURANCE as usize].balance, 0);
    assert!(out.events.iter().any(|e| matches!(e, Event::Fill { kind: FillKind::Adl, .. })));
    assert!(!out.events.iter().any(|e| matches!(e, Event::BadDebt { .. })), "ADL at bankruptcy leaves no debt: {:?}", out.events);
    assert!(r.bal(&a) >= 0);
    assert_eq!(r.e.accounts[ACCT_PROTOCOL_FEE as usize].balance.min(0), 0);
}

/// R395-20 (holds in code since 647a401: nonce sets are keyed by signer and never deleted; stated in SPEC 2.5
/// in 0.2 and pinned here). An agent's applied action cannot be replayed after the agent is revoked and approved
/// again, nor after it expired and was approved again, inside the nonce window.
#[test]
fn r395_20_agent_nonces_survive_revoke_expiry_and_reapproval() {
    let mut r = Rig::one();
    let (ka, kg) = (key(1), key(77));
    let (a, g) = (addr(&ka), addr(&kg));
    let d = r.dep(a, 10_000);
    r.block(3000, &[(1, P)], vec![d]);
    let approve = |r: &Rig, n: u64, exp: u64| sign_action(&r.e, &ka, Action::ApproveAgent { account: a, nonce: r.t + n, agent: g, name: "bot".into(), expires_at: exp });
    let ap = approve(&r, 1, r.t + 3_600_000);
    let order = sign_action(&r.e, &kg, place(a, r.t + 2, 1, true, ORDER_LIMIT, TIF_GTC, P - 100 * TICK, 100_000, false));
    let out = r.block(1000, &[], vec![ap, order.clone()]);
    assert_eq!(out.outcomes, [Ok(()), Ok(())]);
    // revoke, approve again, replay the agent's order in the same block and in a later one
    let rv = sign_action(&r.e, &ka, Action::RevokeAgent { account: a, nonce: r.t + 3, agent: g });
    let ap = approve(&r, 4, r.t + 3_600_000);
    let out = r.block(1000, &[], vec![rv, order.clone(), ap, order.clone()]);
    assert_eq!(out.outcomes, [Ok(()), Err(Error::BadSignature), Ok(()), Err(Error::NonceUsed)], "revoked: not a signer; approved again: the nonce is still used");
    // expiry inside the nonce window: approve until t + 4 s, let it expire, approve again, replay
    let rv = sign_action(&r.e, &ka, Action::RevokeAgent { account: a, nonce: r.t + 5, agent: g });
    let ap = approve(&r, 6, r.t + 1000 + 4000);
    let order2 = sign_action(&r.e, &kg, place(a, r.t + 7, 1, true, ORDER_LIMIT, TIF_GTC, P - 100 * TICK, 100_000, false));
    let out = r.block(1000, &[], vec![rv, ap, order2.clone()]);
    assert_eq!(out.outcomes, [Ok(()), Ok(()), Ok(())]);
    let out = r.block(5000, &[], vec![order2.clone()]);
    assert_eq!(out.outcomes, [Err(Error::BadSignature)], "expired");
    let ap = approve(&r, 8, r.t + 3_600_000);
    let out = r.block(1000, &[], vec![ap, order2]);
    assert_eq!(out.outcomes, [Ok(()), Err(Error::NonceUsed)], "expired, lazily removed, approved again: still single-use");
    assert!(r.e.nonces.contains_key(&g));
}

/// R395-21, the rule (SPEC 2.4, 0.2): accounts appear by deposit and anyone can deposit to any address, so "an
/// agent is not an account" is checked ONLY when an approval is applied; it is not an invariant. A deposit to an
/// approved agent's address makes that address an account as well, and the approval stays valid until it expires
/// or is revoked; the signer is always resolved through `message.account` (S-2.2), so there is no confusion:
/// the address acts for itself with `account = itself` and for its principal with `account = principal`. Cost
/// to the griefed user: a NEW approval (or a renewal) of an address that has become an account is AGENT_INVALID;
/// the user approves a fresh key.
#[test]
fn r395_21_deposit_to_an_agent_address() {
    let mut r = Rig::one();
    let (ka, kg, kb) = (key(1), key(77), key(2));
    let (a, g, b) = (addr(&ka), addr(&kg), addr(&kb));
    let (d1, d2) = (r.dep(a, 10_000), r.dep(b, 10_000));
    r.block(3000, &[(1, P)], vec![d1, d2]);
    let ap = sign_action(&r.e, &ka, Action::ApproveAgent { account: a, nonce: r.t + 1, agent: g, name: "bot".into(), expires_at: r.t + 3_600_000 });
    r.block(1000, &[], vec![ap]);
    // a third party deposits to the agent's address
    let dg = r.dep(g, 50);
    let as_agent = sign_action(&r.e, &kg, place(a, r.t + 1001, 1, true, ORDER_LIMIT, TIF_GTC, P - 100 * TICK, 100_000, false));
    let own = sign_action(&r.e, &kg, Action::Withdraw { account: g, nonce: r.t + 1002, amount: 10_000_000, recipient: g });
    let steal = sign_action(&r.e, &kg, Action::Withdraw { account: a, nonce: r.t + 1003, amount: 10_000_000, recipient: g });
    let renew = sign_action(&r.e, &ka, Action::ApproveAgent { account: a, nonce: r.t + 1004, agent: g, name: "bot".into(), expires_at: r.t + 7_200_000 });
    let other = sign_action(&r.e, &kb, Action::ApproveAgent { account: b, nonce: r.t + 1005, agent: g, name: "x".into(), expires_at: r.t + 7_200_000 });
    let out = r.block(1000, &[], vec![dg, as_agent, own, steal, renew, other]);
    assert_eq!(
        out.outcomes,
        [Ok(()), Ok(()), Ok(()), Err(Error::AgentNotAllowed), Err(Error::AgentInvalid), Err(Error::AgentInvalid)],
        "still a's agent; acts for itself; cannot withdraw for a; cannot be approved anew by anyone"
    );
    assert_eq!(r.e.accounts[r.ai(&a) as usize].orders.len(), 1);
    assert_eq!(r.bal(&g), 40_000_000);
    assert_eq!(r.e.withdrawals[0].account, g);
}

/// R395-22, exact rule S-9.2.1 and the optional genesis `collateralToken`.
#[test]
fn r395_22_recipient_rule_and_collateral_token() {
    let token = "0x00000000000000000000000000000000b05dc000";
    let mut g = genesis_raw(7, vec![market(1, "BTC-USD")]);
    let before = g.encoding();
    assert!(!String::from_utf8(before.clone()).unwrap().contains("collateralToken"), "absent field is not encoded: old genesis hashes stand");
    g.collateral_token = Some(token.into());
    assert_ne!(g.encoding(), before);
    let json = String::from_utf8(g.encoding()).unwrap();
    assert_eq!(Genesis::from_json(&json).unwrap().collateral_token, Some(parse_addr(token).unwrap()));
    // refused at genesis: the vault itself, the fee recipient, a keyless low address
    for bad in [DEV_VAULT, postern_perps_engine::genesis::OWNER_FEE_RECIPIENT, "0x000000000000000000000000000000000000ffff", "0x0000000000000000000000000000000000000000"] {
        let mut x = g.clone();
        x.collateral_token = Some(bad.into());
        assert!(x.check().is_err(), "{bad}");
    }
    let mut r = Rig::new(g);
    let ka = key(1);
    let a = addr(&ka);
    let d = r.dep(a, 1_000);
    r.block(3000, &[(1, P)], vec![d]);
    let mut first_keyed = [0u8; 20];
    first_keyed[17] = 1; // 0x..010000: the first address above the keyless range
    let mut last_keyless = [0u8; 20];
    last_keyless[18] = 0xff;
    last_keyless[19] = 0xff;
    let cases: Vec<([u8; 20], Result<(), Error>)> = vec![
        ([0u8; 20], Err(Error::Malformed)),
        (parse_addr(DEV_VAULT).unwrap(), Err(Error::BadRecipient)),
        (parse_addr(token).unwrap(), Err(Error::BadRecipient)),
        (system_address(ACCT_PROTOCOL_FEE), Err(Error::BadRecipient)),
        (system_address(ACCT_BACKSTOP), Err(Error::BadRecipient)),
        (last_keyless, Err(Error::BadRecipient)),
        (first_keyed, Ok(())),
        (a, Ok(())),
    ];
    let inputs: Vec<Input> = cases.iter().enumerate().map(|(i, (to, _))| sign_action(&r.e, &ka, Action::Withdraw { account: a, nonce: r.t + 1 + i as u64, amount: 10_000_000, recipient: *to })).collect();
    let out = r.block(1000, &[], inputs);
    let want: Vec<Result<(), Error>> = cases.iter().map(|c| c.1).collect();
    assert_eq!(out.outcomes, want);
    assert_eq!(r.e.withdrawals.len(), 2);
    assert_eq!(r.bal(&a), 1_000_000_000 - 20_000_000, "a refused withdrawal debits nothing (its nonce is consumed, S-2.5)");
    assert_eq!(Error::BadRecipient.name(), "BAD_RECIPIENT");
    let _ = hex0x(&a);
}

/// R395-39, the other side: a trigger order reserves nothing, so it does not turn a user-caused refusal into a
/// pending request (a user must not be able to park the forced queue with a free order).
#[test]
fn r395_39_trigger_in_a_halted_market_does_not_make_an_overlarge_request_pending() {
    let mut r = Rig::one();
    let ka = key(1);
    let a = addr(&ka);
    let d = r.dep(a, 10_000);
    r.block(3000, &[(1, P)], vec![d]);
    let mut stop = place(a, r.t + 3000, 1, false, ORDER_STOP_MARKET, TIF_IOC, P - 500 * TICK, 100_000_000, false);
    if let Action::PlaceOrder(p) = &mut stop {
        p.trigger_price = P - 400 * TICK;
    }
    let s = sign_action(&r.e, &ka, stop);
    let out = r.block(3000, &[(1, P)], vec![s]);
    assert_eq!(out.outcomes[1], Ok(()));
    r.block(11_000, &[], vec![]);
    assert!(!r.e.halted_involved(r.ai(&a)));
    let f = r.forced(a, FORCED_WITHDRAW, 1_000_000_000_000);
    let out = r.block(1000, &[], vec![f]);
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedRejected { index: 0, reason: Error::InsufficientMargin })), "{:?}", out.events);
    assert_eq!(r.e.forced_processed, 1);
}
