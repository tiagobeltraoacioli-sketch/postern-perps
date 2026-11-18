//! Round 2 (agent 430), failing-test-first set: every test here uses only the API of `main` f0e3967 and FAILS on
//! that engine (run recorded in agents/430-engine-r2.md); the same tests pass on the fixed engine.
//! R395-39 (forced withdraw vs resting orders in a halted market), R395-22 (stranding recipients), R430-1 (the
//! backstop's negative balance broke the "system accounts never negative" assertion: a reachable state).
mod common;
use common::*;
use postern_perps_engine::engine::{system_address, Event, ACCT_BACKSTOP, ACCT_INSURANCE};
use postern_perps_engine::hash::parse_addr;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

/// R395-39. A resting bid of 1 BTC reserves 6,725 of a's 10,000 USDC (leverage 10 on max(mark, bid) = 67,250); the
/// market halts; a forced withdrawal of 5,000 USDC cannot be served only because the halted market's order holds
/// margin. On f0e3967 it was REJECTED and consumed ("user-caused"); it must stay PENDING (the halt's doing) and be
/// served as soon as the order is gone.
#[test]
fn r395_39_order_in_a_halted_market_makes_a_forced_withdraw_pending_not_rejected() {
    let mut r = Rig::one();
    let ka = key(1);
    let a = addr(&ka);
    let d = r.dep(a, 10_000);
    r.block(3000, &[(1, P)], vec![d]);
    let bid = sign_action(&r.e, &ka, place(a, r.t + 3000, 1, true, ORDER_LIMIT, TIF_GTC, P - 100 * TICK, 100_000_000, false));
    let out = r.block(3000, &[(1, P)], vec![bid]);
    assert_eq!(out.outcomes[1], Ok(()));
    assert_eq!(r.e.free_collateral(r.ai(&a)), 3_275_000_000, "10,000 - 6,725 reserved by the resting bid");
    // halt (no oracle round for longer than maxStalenessMs = 10 s)
    r.block(11_000, &[], vec![]);
    assert!(r.e.accounts[r.ai(&a) as usize].positions.is_empty(), "no position: only the order is in the halted market");
    let f = r.forced(a, FORCED_WITHDRAW, 5_000_000_000);
    let out = r.block(1000, &[], vec![f]);
    assert!(
        out.events.iter().any(|e| matches!(e, Event::ForcedPending { index: 0, reason: Error::PositionInHaltedMarket })),
        "must be pending, got {:?}",
        out.events
    );
    assert_eq!(r.e.forced_processed, 0, "not consumed");
    assert!(r.e.withdrawals.is_empty());
    // the user cancels (allowed in a halt, S-4.10); forced requests are served BEFORE a block's actions (3.3 step
    // 7), so the head is still pending in the block of the cancel and served in the next one
    let c = sign_action(&r.e, &ka, Action::CancelAll { account: a, nonce: r.t + 1000, market_id: 0 });
    let out = r.block(1000, &[], vec![c]);
    assert_eq!(out.outcomes[0], Ok(()));
    let out = r.block(1000, &[], vec![]);
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 0 })), "{:?}", out.events);
    let w = r.e.withdrawals.last().unwrap();
    assert_eq!((w.recipient, w.amount), (a, 4_999_000_000), "5,000 gross minus the 1 USDC fee, to the account itself");
    assert_eq!(r.e.forced_processed, 1);
}

/// R395-22. A signed Withdraw whose recipient is the vault itself was accepted by f0e3967: the record is in the
/// withdrawal tree and the tokens would be paid to the vault (stranded), or, for a keyless low address, never paid
/// at all by the vault (VAULT.md: no record to a recipient <= 0xffff is paid) while the engine has already
/// debited the account.
#[test]
fn r395_22_withdraw_to_the_vault_or_a_keyless_address_is_refused() {
    let mut r = Rig::one();
    let ka = key(1);
    let a = addr(&ka);
    let d = r.dep(a, 1_000);
    r.block(3000, &[(1, P)], vec![d]);
    let vault = parse_addr(DEV_VAULT).unwrap();
    let mut keyless = [0u8; 20];
    keyless[18] = 0xff;
    keyless[19] = 0xff;
    let w = |r: &Rig, n: u64, to| sign_action(&r.e, &ka, Action::Withdraw { account: a, nonce: r.t + n, amount: 100_000_000, recipient: to });
    let (w1, w2, w3) = (w(&r, 1, vault), w(&r, 2, system_address(ACCT_INSURANCE)), w(&r, 3, keyless));
    let out = r.block(1000, &[], vec![w1, w2, w3]);
    for (i, o) in out.outcomes.iter().enumerate() {
        assert!(o.is_err(), "withdrawal {i} to a stranding recipient was accepted");
    }
    assert!(r.e.withdrawals.is_empty());
    assert_eq!(r.bal(&a), 1_000_000_000, "nothing debited");
}

/// R430-1 (found in round 2 while specifying R395-17). The backstop takes a liquidated long, the price keeps
/// falling, and a forced close of the short deleverages the backstop first (R395-14): it realises a loss larger
/// than its balance. On f0e3967 `invariants::check` then fails ("system account 2 negative"): the assertion was
/// false for a reachable state. Fixed rule (SPEC 8, 0.2): BACKSTOP's negative balance is recorded bad debt, paid by
/// INSURANCE first once the backstop is flat; PROTOCOL_FEE and INSURANCE still never go negative.
#[test]
fn r430_1_backstop_loss_beyond_its_balance_is_bad_debt_paid_by_insurance() {
    let mut r = Rig::one();
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let (d1, d2, d3) = (r.dep(a, 700), r.dep(b, 50_000), r.dep(system_address(ACCT_BACKSTOP), 100));
    r.block(3000, &[(1, P)], vec![d1, d2, d3]);
    r.trade(&[(1, P)], 1, &kb, &ka, P, 10_000_000); // a long 0.1 BTC, b short
    let mut p = P;
    let mut liquidated = false;
    for _ in 0..12 {
        p = tick(p * 985 / 1000);
        let out = r.block(3000, &[(1, p)], vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })) {
            liquidated = true;
            break;
        }
    }
    assert!(liquidated);
    assert_eq!(r.size(ACCT_BACKSTOP, 1), 10_000_000, "the backstop took a's long");
    for _ in 0..10 {
        p = tick(p * 96 / 100);
        r.block(3000, &[(1, p)], vec![]);
    }
    assert!(r.e.equity(ACCT_BACKSTOP) < 0, "the backstop is under water and, by rule, never liquidated");
    // b exits by the forced path: the backstop is its ADL counterparty and realises the loss
    let f = r.forced(b, FORCED_CLOSE, 0);
    let out = r.block(3000, &[(1, p)], vec![f]); // on f0e3967 the rig's invariant check panics here
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 0 })));
    assert_eq!(r.size(ACCT_BACKSTOP, 1), 0);
    let debt = -r.e.accounts[ACCT_BACKSTOP as usize].balance;
    assert!(debt > 0, "loss beyond the 100 USDC seed");
    assert!(out.events.iter().any(|e| matches!(e, Event::BadDebt { account: ACCT_BACKSTOP, amount } if *amount == debt)), "recorded, never hidden: {:?}", out.events);
    assert_eq!(r.e.accounts[ACCT_INSURANCE as usize].balance, 0, "INSURANCE never goes below zero");
    // the escape values charge the deficit: total never above custody once the fund covers it
    let ins = r.dep(system_address(ACCT_INSURANCE), 5_000);
    let out = r.block(3000, &[(1, p)], vec![ins]);
    assert!(out.events.iter().any(|e| matches!(e, Event::InsuranceCover { account: ACCT_BACKSTOP, amount } if *amount == debt)), "{:?}", out.events);
    assert_eq!(r.e.accounts[ACCT_BACKSTOP as usize].balance, 0);
    let left = r.e.accounts[ACCT_INSURANCE as usize].balance;
    assert!(left >= 5_000_000_000 - debt && left < 5_000_000_000 - debt + 10, "the fund paid exactly the debt (plus at most swept dust): {left}");
}

/// R430-2 (found in round 2 while running 394's `max-values` vector through the engine). A committee round whose
/// price for a listed market is above MAX_PRICE (10^15, SPEC section 1) was accepted by f0e3967 as the index: the
/// bound that every i128 product of section 1 relies on did not hold for the index (the mark was clamped, the
/// index and the exit band were not). Fixed: such an observation does not count for the market (no committee
/// price; the market stays halted / goes stale: fail closed).
#[test]
fn r430_2_committee_price_above_max_price_never_becomes_the_index() {
    let mut r = Rig::one();
    let over = postern_perps_engine::num::MAX_PRICE + TICK;
    let out = r.block(3000, &[(1, over)], vec![]);
    assert_eq!(out.outcomes, [Ok(())], "the update itself is well-formed and signed");
    let m = &r.e.markets[&1];
    assert!(m.oracle.index <= postern_perps_engine::num::MAX_PRICE, "index {} above MAX_PRICE", m.oracle.index);
    assert_ne!(m.oracle.status, postern_perps_engine::oracle::Status::Active, "no price, no trading");
    // the bound itself is still a valid price
    let out = r.block(3000, &[(1, postern_perps_engine::num::MAX_PRICE)], vec![]);
    assert_eq!(out.outcomes, [Ok(())]);
    assert_eq!(r.e.markets[&1].oracle.index, postern_perps_engine::num::MAX_PRICE);
}
