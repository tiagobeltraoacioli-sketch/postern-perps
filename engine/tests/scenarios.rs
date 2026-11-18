//! Scenario tests: every test checks the invariants (SPEC section 8) after every block.
use postern_perps_engine::engine::{Event, FillKind, ACCT_BACKSTOP, ACCT_INSURANCE, ACCT_PROTOCOL_FEE, WITHDRAWAL_DEPTH};
use postern_perps_engine::hash::Addr;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;
use postern_perps_engine::{invariants, merkle};

const T0: u64 = 1_794_880_000_000;
const BTC: u32 = 1;
const P67250: u64 = 6_725_000_000_000;

struct Rig {
    e: Engine,
    ok: Vec<SigningKey>,
    h: u64,
    t: u64,
    dep: u64,
}
use k256::ecdsa::SigningKey;

impl Rig {
    fn new() -> Rig {
        let e = Engine::new(genesis(7, vec![market(BTC, "BTC-USD")]));
        Rig { e, ok: oracle_keys(7), h: 0, t: T0, dep: 0 }
    }
    fn block(&mut self, dt: u64, inputs: Vec<Input>) -> BlockOutcome {
        self.h += 1;
        self.t += dt;
        let out = self.e.apply_block(&Block { height: self.h, time: self.t, inputs }).expect("block applies");
        invariants::check(&self.e).expect("invariants hold");
        out
    }
    fn oracle(&self, p: u64) -> Input {
        oracle_round(&self.e, &self.ok, self.t, &[(BTC, p)])
    }
    fn deposit(&mut self, a: Addr, amount: u64) -> Input {
        let i = Input::Deposit { index: self.dep, account: a, amount };
        self.dep += 1;
        i
    }
    /// next block time with an oracle round at it
    fn tick(&mut self, dt: u64, p: u64, mut rest: Vec<Input>) -> BlockOutcome {
        self.h += 1;
        self.t += dt;
        let mut inputs = vec![oracle_round(&self.e, &self.ok, self.t, &[(BTC, p)])];
        inputs.append(&mut rest);
        let out = self.e.apply_block(&Block { height: self.h, time: self.t, inputs }).expect("block applies");
        invariants::check(&self.e).expect("invariants hold");
        out
    }
}

fn usdc(x: u64) -> u64 {
    x * 1_000_000
}

fn ai(e: &Engine, a: &Addr) -> usize {
    e.by_addr[a] as usize
}

#[test]
fn worked_example_fee_and_fill() {
    let mut r = Rig::new();
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let d1 = r.deposit(a, usdc(10_000));
    let d2 = r.deposit(b, usdc(10_000));
    let out = r.tick(1000, P67250, vec![d1, d2]);
    assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
    let t = r.t + 1000;
    let maker = sign_action(&r.e, &kb, place(b, t, BTC, false, ORDER_LIMIT, TIF_GTC, P67250, 50_000_000, false));
    let taker = sign_action(&r.e, &ka, place(a, t, BTC, true, ORDER_MARKET, TIF_IOC, P67250 + 1_000_000_000, 50_000_000, false));
    let out = r.block(1000, vec![maker, taker]);
    assert_eq!(out.outcomes, vec![Ok(()), Ok(())]);
    // SPEC section 15: taker fee 15.13125 USDC, maker fee 450/150 of it rounded up
    let fill = out.events.iter().find_map(|e| match e {
        Event::Fill { taker_fee, maker_fee, price, size, kind: FillKind::Book, .. } => Some((*taker_fee, *maker_fee, *price, *size)),
        _ => None,
    });
    assert_eq!(fill, Some((15_131_250, 5_043_750, P67250, 50_000_000)));
    assert_eq!(r.e.accounts[ACCT_PROTOCOL_FEE as usize].balance, 15_131_250 + 5_043_750);
    assert_eq!(r.e.accounts[ai(&r.e, &a)].positions[&BTC].size, 50_000_000);
    assert_eq!(r.e.accounts[ai(&r.e, &b)].positions[&BTC].size, -50_000_000);
}

#[test]
fn signatures_nonces_and_agents() {
    let mut r = Rig::new();
    let (ka, kagent, kx) = (key(1), key(50), key(51));
    let a = addr(&ka);
    let d = r.deposit(a, usdc(1000));
    r.tick(1000, P67250, vec![d]);
    let t = r.t + 1000;
    // tampered signature
    let mut bad = sign_action(&r.e, &ka, place(a, t, BTC, true, ORDER_LIMIT, TIF_GTC, P67250 - 100_000_000_000, 100_000, false));
    if let Input::Action(sa) = &mut bad {
        sa.signature[10] ^= 1;
    }
    // signed by a stranger for account a
    let stranger = sign_action(&r.e, &kx, place(a, t + 1, BTC, true, ORDER_LIMIT, TIF_GTC, P67250 - 100_000_000_000, 100_000, false));
    let good = sign_action(&r.e, &ka, place(a, t + 2, BTC, true, ORDER_LIMIT, TIF_GTC, P67250 - 100_000_000_000, 100_000, false));
    let replay = good.clone();
    let stale = sign_action(&r.e, &ka, place(a, t - 130_000, BTC, true, ORDER_LIMIT, TIF_GTC, P67250 - 100_000_000_000, 100_000, false));
    let future = sign_action(&r.e, &ka, place(a, t + 31_000, BTC, true, ORDER_LIMIT, TIF_GTC, P67250 - 100_000_000_000, 100_000, false));
    let zero_price_market = sign_action(&r.e, &ka, place(a, t + 3, BTC, true, ORDER_MARKET, TIF_IOC, 0, 100_000, false));
    let out = r.block(1000, vec![bad, stranger, good, replay, stale, future, zero_price_market]);
    assert_eq!(
        out.outcomes,
        vec![Err(Error::BadSignature), Err(Error::BadSignature), Ok(()), Err(Error::NonceUsed), Err(Error::NonceWindow), Err(Error::NonceWindow), Err(Error::Malformed)]
    );
    // agent approval; agent may trade, may not withdraw or approve
    let t = r.t + 1000;
    let ag = addr(&kagent);
    let approve = sign_action(&r.e, &ka, Action::ApproveAgent { account: a, nonce: t, agent: ag, name: "bot".into(), expires_at: t + 3_600_000 });
    let by_agent = sign_action(&r.e, &kagent, place(a, t + 1, BTC, true, ORDER_LIMIT, TIF_GTC, P67250 - 200_000_000_000, 100_000, false));
    let agent_withdraw = sign_action(&r.e, &kagent, Action::Withdraw { account: a, nonce: t + 2, amount: usdc(10), recipient: ag });
    let out = r.block(1000, vec![approve, by_agent, agent_withdraw]);
    assert_eq!(out.outcomes, vec![Ok(()), Ok(()), Err(Error::AgentNotAllowed)]);
    // revoke, then the agent is refused
    let t = r.t + 1000;
    let revoke = sign_action(&r.e, &ka, Action::RevokeAgent { account: a, nonce: t, agent: ag });
    let after = sign_action(&r.e, &kagent, place(a, t + 1, BTC, true, ORDER_LIMIT, TIF_GTC, P67250 - 200_000_000_000, 100_000, false));
    let out = r.block(1000, vec![revoke, after]);
    assert_eq!(out.outcomes, vec![Ok(()), Err(Error::BadSignature)]);
}

#[test]
fn out_of_order_nonces_within_the_set_are_accepted() {
    let mut r = Rig::new();
    let ka = key(1);
    let a = addr(&ka);
    let d = r.deposit(a, usdc(1000));
    r.tick(1000, P67250, vec![d]);
    let t = r.t + 1000;
    let p = P67250 - 100_000_000_000;
    let later = sign_action(&r.e, &ka, place(a, t + 5, BTC, true, ORDER_LIMIT, TIF_GTC, p, 100_000, false));
    let earlier = sign_action(&r.e, &ka, place(a, t + 4, BTC, true, ORDER_LIMIT, TIF_GTC, p, 100_000, false));
    let out = r.block(1000, vec![later, earlier]);
    assert_eq!(out.outcomes, vec![Ok(()), Ok(())]);
}

#[test]
fn block_level_refusals_change_nothing() {
    let mut r = Rig::new();
    let root0 = r.e.state_root();
    let a = addr(&key(1));
    assert_eq!(r.e.apply_block(&Block { height: 2, time: T0, inputs: vec![] }).unwrap_err(), BlockError::Height);
    let dep = Input::Deposit { index: 0, account: a, amount: 1 };
    let orc = r.oracle(P67250);
    assert_eq!(r.e.apply_block(&Block { height: 1, time: T0, inputs: vec![dep, orc] }).unwrap_err(), BlockError::Order);
    assert_eq!(r.e.state_root(), root0);
    r.block(1, vec![]);
    assert_eq!(r.e.apply_block(&Block { height: 2, time: r.t, inputs: vec![] }).unwrap_err(), BlockError::Time);
}

#[test]
fn deposits_in_vault_order_only() {
    let mut r = Rig::new();
    let a = addr(&key(1));
    let out = r.block(1, vec![Input::Deposit { index: 1, account: a, amount: 5 }, Input::Deposit { index: 0, account: a, amount: 5 }]);
    assert_eq!(out.outcomes, vec![Err(Error::DepositOrder), Ok(())]);
}

#[test]
fn withdrawal_record_is_provable_and_fee_goes_to_protocol() {
    let mut r = Rig::new();
    let ka = key(1);
    let a = addr(&ka);
    let d = r.deposit(a, usdc(100));
    r.tick(1000, P67250, vec![d]);
    let t = r.t + 1000;
    let recipient = addr(&key(9));
    let too_small = sign_action(&r.e, &ka, Action::Withdraw { account: a, nonce: t, amount: 1_000_000, recipient });
    let w = sign_action(&r.e, &ka, Action::Withdraw { account: a, nonce: t + 1, amount: usdc(40), recipient });
    let out = r.block(1000, vec![too_small, w]);
    assert_eq!(out.outcomes, vec![Err(Error::WithdrawTooSmall), Ok(())]);
    assert_eq!(r.e.withdrawals.len(), 1);
    assert_eq!(r.e.withdrawals[0].amount, usdc(39));
    assert_eq!(r.e.accounts[ACCT_PROTOCOL_FEE as usize].balance, 1_000_000);
    let (data, proof) = r.e.withdrawal_proof(0).unwrap();
    let root = merkle::root(&r.e.withdrawal_leaves(), WITHDRAWAL_DEPTH);
    assert!(merkle::verify(&merkle::leaf_hash(&data), 0, &proof, &root));
    // account proof
    let (adata, idx, ap) = r.e.account_proof(&a).unwrap();
    let aroot = merkle::root(&r.e.account_leaves(), 32);
    assert!(merkle::verify(&merkle::leaf_hash(&adata), idx as u64, &ap, &aroot));
}

#[test]
fn post_only_and_reduce_only() {
    let mut r = Rig::new();
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let d1 = r.deposit(a, usdc(10_000));
    let d2 = r.deposit(b, usdc(10_000));
    r.tick(1000, P67250, vec![d1, d2]);
    let t = r.t + 1000;
    let ask = sign_action(&r.e, &kb, place(b, t, BTC, false, ORDER_LIMIT, TIF_GTC, P67250, 100_000, false));
    let cross_po = sign_action(&r.e, &ka, place(a, t, BTC, true, ORDER_LIMIT, TIF_POST_ONLY, P67250, 100_000, false));
    let ro_nothing = sign_action(&r.e, &ka, place(a, t + 1, BTC, true, ORDER_LIMIT, TIF_GTC, P67250, 100_000, true));
    let out = r.block(1000, vec![ask, cross_po, ro_nothing]);
    assert_eq!(out.outcomes, vec![Ok(()), Err(Error::WouldCross), Err(Error::ReduceOnly)]);
}

#[test]
fn insufficient_margin_is_refused_before_any_fill() {
    let mut r = Rig::new();
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let d1 = r.deposit(a, usdc(100));
    let d2 = r.deposit(b, usdc(100_000));
    r.tick(1000, P67250, vec![d1, d2]);
    let t = r.t + 1000;
    // 1 BTC at 10x needs 6725 USDC; a has 100
    let ask = sign_action(&r.e, &kb, place(b, t, BTC, false, ORDER_LIMIT, TIF_GTC, P67250, 100_000_000, false));
    let buy = sign_action(&r.e, &ka, place(a, t, BTC, true, ORDER_LIMIT, TIF_GTC, P67250, 100_000_000, false));
    let mkt = sign_action(&r.e, &ka, place(a, t + 1, BTC, true, ORDER_MARKET, TIF_IOC, P67250, 100_000_000, false));
    let out = r.block(1000, vec![ask, buy, mkt]);
    assert_eq!(out.outcomes, vec![Ok(()), Err(Error::InsufficientMargin), Ok(())]);
    // the market order filled only what margin allowed (maybe nothing), never more
    let pos = r.e.accounts[ai(&r.e, &a)].positions.get(&BTC).map(|p| p.size).unwrap_or(0);
    assert!(pos <= 1_000_000, "at most 0.01 BTC with 100 USDC at 10x, got {pos}");
}

#[test]
fn liquidation_through_book_backstop_and_adl_conserves_value() {
    let mut r = Rig::new();
    let (ka, kb, kc) = (key(1), key(2), key(3));
    let (a, b, c) = (addr(&ka), addr(&kb), addr(&kc));
    let d1 = r.deposit(a, usdc(700));
    let d2 = r.deposit(b, usdc(50_000));
    let d3 = r.deposit(c, usdc(50_000));
    r.tick(1000, P67250, vec![d1, d2, d3]);
    let t = r.t + 1000;
    // a goes long 0.1 BTC at 10x (6725 notional, 672.5 IR) against b
    let ask = sign_action(&r.e, &kb, place(b, t, BTC, false, ORDER_LIMIT, TIF_GTC, P67250, 10_000_000, false));
    let buy = sign_action(&r.e, &ka, place(a, t, BTC, true, ORDER_MARKET, TIF_IOC, P67250 + 100_000_000_000, 10_000_000, false));
    let out = r.block(1000, vec![ask, buy]);
    assert_eq!(out.outcomes, vec![Ok(()), Ok(())]);
    assert_eq!(r.e.accounts[ai(&r.e, &a)].positions[&BTC].size, 10_000_000);
    // price falls 4.5% in steps inside the jump bound: a's equity falls below 1% maintenance
    let mut p = P67250;
    let mut liquidated_at = None;
    for step in 0..10 {
        p = p * 985 / 1000 / 10_000_000 * 10_000_000;
        let out = r.tick(3000, p, vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })) {
            liquidated_at = Some(step);
            break;
        }
    }
    assert!(liquidated_at.is_some(), "a is liquidated once equity < maintenance");
    let a_i = ai(&r.e, &a);
    assert!(r.e.accounts[a_i].positions.get(&BTC).is_none(), "a liquidated (no bids, no backstop capital: ADL)");
    let adl = r.e.accounts[ai(&r.e, &b)].positions.get(&BTC).map(|x| x.size).unwrap_or(0);
    assert_eq!(adl, 0, "b, the only short, was deleveraged against a's long");
    let _ = c;
    let _ = (ACCT_INSURANCE, ACCT_BACKSTOP);
}

#[test]
fn funding_is_paid_at_the_boundary_and_conserves() {
    let mut r = Rig::new();
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let d1 = r.deposit(a, usdc(100_000));
    let d2 = r.deposit(b, usdc(100_000));
    // start just before an hour boundary
    let to_boundary = 3_600_000 - (r.t % 3_600_000) - 10_000;
    r.tick(to_boundary, P67250, vec![d1, d2]);
    let t = r.t + 1000;
    let ask = sign_action(&r.e, &kb, place(b, t, BTC, false, ORDER_LIMIT, TIF_GTC, P67250, 100_000_000, false));
    let buy = sign_action(&r.e, &ka, place(a, t, BTC, true, ORDER_MARKET, TIF_IOC, P67250 + 100_000_000_000, 100_000_000, false));
    r.tick(3000, P67250, vec![ask, buy]);
    let before = (r.e.accounts[ai(&r.e, &a)].balance, r.e.accounts[ai(&r.e, &b)].balance);
    let mut paid = false;
    for _ in 0..5 {
        let out = r.tick(3000, P67250, vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Funding { .. })) {
            paid = true;
        }
    }
    assert!(paid, "funding settled at the boundary");
    let after = (r.e.accounts[ai(&r.e, &a)].balance, r.e.accounts[ai(&r.e, &b)].balance);
    // empty book -> premium 0 -> rate = interest (positive): the long pays, the short receives
    assert!(after.0 < before.0 && after.1 > before.1, "{before:?} -> {after:?}");
    assert!(before.0 - after.0 >= after.1 - before.1, "payer pays at least what the receiver gets");
}

#[test]
fn halted_market_refuses_orders_and_withdrawal_with_position() {
    let mut r = Rig::new();
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let d1 = r.deposit(a, usdc(10_000));
    let d2 = r.deposit(b, usdc(10_000));
    r.tick(1000, P67250, vec![d1, d2]);
    let t = r.t + 1000;
    let ask = sign_action(&r.e, &kb, place(b, t, BTC, false, ORDER_LIMIT, TIF_GTC, P67250, 1_000_000, false));
    let buy = sign_action(&r.e, &ka, place(a, t, BTC, true, ORDER_MARKET, TIF_IOC, P67250 + 1_000_000_000, 1_000_000, false));
    r.block(1000, vec![ask, buy]);
    // no oracle for > 10 s: stale halt
    r.block(11_000, vec![]);
    let t = r.t + 1000;
    let o = sign_action(&r.e, &kb, place(b, t, BTC, false, ORDER_LIMIT, TIF_GTC, P67250, 1_000_000, false));
    // Restated for R395-5 (SPEC 0.1.3): a halt no longer blocks the withdrawal of FREE collateral. The old property
    // (a halted market accepts no new order) is still asserted; a withdrawal beyond free collateral is still refused.
    let w = sign_action(&r.e, &ka, Action::Withdraw { account: a, nonce: t, amount: usdc(10), recipient: a });
    let all = sign_action(&r.e, &ka, Action::Withdraw { account: a, nonce: t + 1, amount: usdc(10_000), recipient: a });
    let out = r.block(1000, vec![o, w, all]);
    assert_eq!(out.outcomes, vec![Err(Error::MarketHalted), Ok(()), Err(Error::InsufficientMargin)]);
}

#[test]
fn replay_is_deterministic() {
    let build = || {
        let mut r = Rig::new();
        let (ka, kb) = (key(1), key(2));
        let (a, b) = (addr(&ka), addr(&kb));
        let d1 = r.deposit(a, usdc(10_000));
        let d2 = r.deposit(b, usdc(10_000));
        r.tick(1000, P67250, vec![d1, d2]);
        let t = r.t + 1000;
        let ask = sign_action(&r.e, &kb, place(b, t, BTC, false, ORDER_LIMIT, TIF_GTC, P67250, 1_000_000, false));
        let buy = sign_action(&r.e, &ka, place(a, t, BTC, true, ORDER_MARKET, TIF_IOC, P67250 + 1_000_000_000, 500_000, false));
        let out = r.tick(1000, P67250 + 10_000_000, vec![ask, buy]);
        (out.state_root, out.log_hash)
    };
    assert_eq!(build(), build());
}
