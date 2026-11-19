//! Fixes of reviewer 395's High findings R395-1 and R395-5 and of R395-31 (SPEC 0.1.3).
use k256::ecdsa::SigningKey;
use postern_perps_engine::engine::{self, system_address, Event, ACCT_INSURANCE};
use postern_perps_engine::hash::{keccak, Addr, H256};
use postern_perps_engine::invariants;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

const T0: u64 = 1_794_880_000_000;
const P: u64 = 6_725_000_000_000;

struct Rig {
    e: Engine,
    ok: Vec<SigningKey>,
    h: u64,
    t: u64,
    dep: u64,
    fid: u64,
}

impl Rig {
    fn new() -> Rig {
        Rig { e: Engine::new(genesis(7, vec![market(1, "BTC-USD")])), ok: oracle_keys(7), h: 0, t: T0, dep: 0, fid: 0 }
    }
    fn dep(&mut self, a: Addr, usdc: u64) -> Input {
        let i = Input::Deposit { index: self.dep, account: a, amount: usdc * 1_000_000 };
        self.dep += 1;
        i
    }
    fn forced(&mut self, a: Addr, kind: u8, _market_id: u32, arg: u64) -> Input {
        let i = Input::Forced { index: self.fid, account: a, kind, deadline: T0 / 1000 + 86_400, arg };
        self.fid += 1;
        i
    }
    fn block(&mut self, dt: u64, oracle: Option<u64>, mut rest: Vec<Input>) -> BlockOutcome {
        self.h += 1;
        self.t += dt;
        let mut inputs = vec![];
        if let Some(p) = oracle {
            inputs.push(oracle_round(&self.e, &self.ok, self.t, &[(1, p)]));
        }
        inputs.append(&mut rest);
        let out = self.e.apply_block(&Block { height: self.h, time: self.t, inputs }).unwrap();
        invariants::check(&self.e).unwrap();
        out
    }
    /// a (long 0.1 BTC at P, 700 USDC) against b (short, 50,000 USDC)
    fn open(&mut self, a_usdc: u64) -> (SigningKey, SigningKey) {
        let (ka, kb) = (key(1), key(2));
        let (d1, d2) = (self.dep(addr(&ka), a_usdc), self.dep(addr(&kb), 50_000));
        self.block(1000, Some(P), vec![d1, d2]);
        let t = self.t + 1000;
        let ask = sign_action(&self.e, &kb, place(addr(&kb), t, 1, false, ORDER_LIMIT, TIF_GTC, P, 10_000_000, false));
        let buy = sign_action(&self.e, &ka, place(addr(&ka), t, 1, true, ORDER_MARKET, TIF_IOC, P + 1_000_000_000, 10_000_000, false));
        let out = self.block(3000, Some(P), vec![ask, buy]); // a new 3 s oracle round
        assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
        (ka, kb)
    }
}

/// Independent recomputation of VAULT.md 5.4 (abi.encode words built here, not with the engine's helpers).
fn vault_deposit_heads(deposits: &[(Addr, u64)]) -> Vec<H256> {
    let word = |x: u128| {
        let mut w = [0u8; 32];
        w[16..].copy_from_slice(&x.to_be_bytes());
        w
    };
    let mut tag = [0u8; 32];
    tag[..23].copy_from_slice(b"PosternPerps.deposit.v1");
    let mut vault = [0u8; 32];
    vault[12..].copy_from_slice(&hex::decode(&DEV_VAULT[2..]).unwrap());
    let mut h = keccak(&[tag, word(700770), vault].concat());
    let mut heads = vec![h];
    for (i, (a, amt)) in deposits.iter().enumerate() {
        let mut aw = [0u8; 32];
        aw[12..].copy_from_slice(a);
        h = keccak(&[h, word(i as u128), aw, word(*amt as u128)].concat());
        heads.push(h);
    }
    heads
}

#[test]
fn r395_1_engine_computes_deposits_head_and_a_fake_deposit_cannot_reach_a_checkpoint() {
    let (a, b) = (addr(&key(1)), addr(&key(2)));
    // what the vault really holds: two deposits
    let real = [(a, 1_000_000_000u64), (b, 2_000_000)];
    let vault_heads = vault_deposit_heads(&real);
    // honest sequencer
    let mut honest = Rig::new();
    honest.block(1000, None, vec![Input::Deposit { index: 0, account: a, amount: real[0].1 }, Input::Deposit { index: 1, account: b, amount: real[1].1 }]);
    let cp = honest.e.checkpoint_fields();
    assert_eq!((cp.deposits_credited, cp.deposits_head), (2, vault_heads[2]), "engine head = vault head");
    // lying sequencer: deposit #1 altered (attacker credits itself 10M); every validator replays it the same way
    let mut lying = Rig::new();
    let attacker = addr(&key(66));
    lying.block(1000, None, vec![Input::Deposit { index: 0, account: a, amount: real[0].1 }, Input::Deposit { index: 1, account: attacker, amount: 10_000_000_000_000 }]);
    let cp = lying.e.checkpoint_fields();
    assert_eq!(cp.deposits_credited, 2);
    // the vault's acceptance rule (VAULT.md 5.3 rule 2): depositsHead == depositHead(depositsCredited)
    assert_ne!(cp.deposits_head, vault_heads[cp.deposits_credited as usize], "the vault refuses this checkpoint");
    // and an invented third deposit is refused by count alone
    let mut extra = Rig::new();
    extra.block(1000, None, vec![
        Input::Deposit { index: 0, account: a, amount: real[0].1 },
        Input::Deposit { index: 1, account: b, amount: real[1].1 },
        Input::Deposit { index: 2, account: attacker, amount: 5 },
    ]);
    assert!(extra.e.checkpoint_fields().deposits_credited as usize > real.len(), "depositsCredited > depositCount: refused by VAULT 5.3 rule 2");
}

#[test]
fn r395_5_halt_never_blocks_free_collateral_withdrawal() {
    let mut r = Rig::new();
    let (ka, _kb) = r.open(10_000);
    let a = addr(&ka);
    r.block(11_000, None, vec![]); // no oracle for > maxStaleness: halted
    assert!(!r.e.markets[&1].oracle.status.eq(&postern_perps_engine::oracle::Status::Active));
    let free = r.e.free_collateral(r.e.by_addr[&a]);
    assert!(free > 8_000_000_000, "most of 10,000 USDC is free with a 0.1 BTC position: {free}");
    let t = r.t + 1000;
    let w = sign_action(&r.e, &ka, Action::Withdraw { account: a, nonce: t, amount: free as u64, recipient: a });
    let over = sign_action(&r.e, &ka, Action::Withdraw { account: a, nonce: t + 1, amount: 2_000_000, recipient: a });
    let out = r.block(1000, None, vec![w, over]);
    assert_eq!(out.outcomes, vec![Ok(()), Err(Error::InsufficientMargin)]);
}

/// Restated at SPEC 0.1.4 (394's exit band, coordinator 2026-11-17): a forced close during a halt is SERVED at
/// once, the book inside the exit band, the rest against the opposite positions; it is no longer left pending. The
/// property of 0.1.3 (a halt never consumes a forced request without serving it) is kept: it is served, not
/// rejected.
/// Restated again at SPEC 0.2.3 (review 436, R436-10 High; 432's corrected README 6a): the price of that close is
/// no longer `settle` (the frozen index: a free exit at the pre-halt price for whoever controls the halt) but the
/// side of the exit band WORSE for the closer (a long at `low`). Kept from the old test: both requests are served
/// and processed, the position is closed against the opposite position, nothing is rejected. The function keeps
/// its round-1 name so that the test count and history stay comparable.
#[test]
fn r395_5_forced_close_in_a_halt_is_served_at_the_settle_price() {
    let mut r = Rig::new();
    let (ka, kb) = r.open(10_000);
    let (a, b) = (addr(&ka), addr(&kb));
    r.block(11_000, None, vec![]); // halted (stale)
    let x = r.e.exit_price(1).unwrap();
    assert!(x.low < P && x.high > P && x.settle == P, "band around the last index, settle = last index");
    let f = r.forced(a, FORCED_CLOSE, 0, 0);
    let f2 = r.forced(b, FORCED_CANCEL_ALL, 0, 0);
    let out = r.block(1000, None, vec![f, f2]);
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 0 })));
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 1 })));
    assert_eq!(r.e.forced_processed, 2);
    let fill = out.events.iter().find_map(|e| match e { Event::Fill { price, kind: engine::FillKind::Adl, .. } => Some(*price), _ => None });
    let low = r.e.exit_price(1).unwrap().low;
    assert_eq!(fill, Some(low), "a long closes against the opposite position at the LOW side of the exit band (0.2.3), not at settle");
    assert!(low < P);
    assert!(r.e.accounts[r.e.by_addr[&a] as usize].positions.is_empty());
}

/// Restated at SPEC 0.2.3 (R436-10, 432's rule): X3 settles a market halted for `maxHaltMs` only at a price the
/// committee signed SINCE the halt. Here the halt is a jump (the committee signs 6% higher, which is that price);
/// after an hour of silence every position settles there. The round-1 form of this test halted by silence alone
/// and expected a settlement at the frozen pre-halt index: that is exactly the free exit R436-10 describes, and
/// `r436_10_silence_alone_never_settles_a_market` (tests/r2_r436_failing_first.rs) now pins the opposite. Kept
/// from the old test: after `maxHaltMs` the market is settled, every position is closed, open interest is zero and
/// the whole balance is free collateral.
#[test]
fn r395_5_a_halt_longer_than_max_halt_settles_the_market() {
    let mut r = Rig::new();
    let (ka, kb) = r.open(10_000);
    let (a, b) = (addr(&ka), addr(&kb));
    let jump = P * 106 / 100 / 10_000_000 * 10_000_000;
    r.block(3000, Some(jump), vec![]);
    assert_eq!(r.e.markets[&1].oracle.status, postern_perps_engine::oracle::Status::Halted(postern_perps_engine::oracle::HaltReason::Jump));
    assert_eq!(r.e.markets[&1].oracle.index, P, "the index did not move");
    let out = r.block(3_600_000, None, vec![]);
    assert!(out.events.iter().any(|e| matches!(e, Event::MarketSettled { market: 1, price } if *price == jump)), "{:?}", out.events);
    for x in [a, b] {
        assert!(r.e.accounts[r.e.by_addr[&x] as usize].positions.is_empty());
    }
    assert_eq!(r.e.markets[&1].long_oi, 0);
    // then everything is free collateral
    let free = r.e.free_collateral(r.e.by_addr[&a]);
    assert_eq!(free, r.e.accounts[r.e.by_addr[&a] as usize].balance);
}

#[test]
fn r395_5_forced_withdraw_rules() {
    let mut r = Rig::new();
    let (ka, _kb) = r.open(10_000);
    let a = addr(&ka);
    // active market, more than the account has: a user-caused, provable refusal is consumed (else any depositor
    // could stall the queue into a global escape)
    let f = r.forced(a, FORCED_WITHDRAW, 0, 1_000_000_000_000);
    let out = r.block(1000, Some(P), vec![f]);
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedRejected { index: 0, reason: Error::InsufficientMargin })));
    assert_eq!(r.e.forced_processed, 1);
    // halted market, the same over-large request: halt-caused, stays pending
    r.block(11_000, None, vec![]);
    let f = r.forced(a, FORCED_WITHDRAW, 0, 1_000_000_000_000);
    let out = r.block(1000, None, vec![f]);
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedPending { index: 1, .. })));
    assert_eq!(r.e.forced_processed, 1);
}

#[test]
fn r395_5_forced_withdraw_everything_during_a_halt_pays_free_collateral() {
    let mut r = Rig::new();
    let (ka, _kb) = r.open(10_000);
    let a = addr(&ka);
    r.block(11_000, None, vec![]);
    let free = r.e.free_collateral(r.e.by_addr[&a]);
    let f = r.forced(a, FORCED_WITHDRAW, 0, 0);
    let out = r.block(1000, None, vec![f]);
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 0 })));
    let w = r.e.withdrawals.last().unwrap();
    assert_eq!((w.recipient, w.amount as i128), (a, free - 1_000_000), "everything free, minus the flat fee, to the account itself");
}

#[test]
fn r395_5_forced_request_before_the_deposit_is_credited_waits() {
    let mut r = Rig::new();
    let x = addr(&key(9));
    let f = r.forced(x, FORCED_CANCEL_ALL, 0, 0);
    r.block(1000, Some(P), vec![f]);
    assert_eq!(r.e.forced_processed, 0, "an uncredited deposit is the sequencer's delay, not the user's fault");
    let d = r.dep(x, 100);
    let out = r.block(1000, Some(P), vec![d]);
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 0 })));
}

#[test]
fn r395_31_gap_liquidation_draws_insurance_first() {
    let mut r = Rig::new();
    let ins = r.dep(system_address(ACCT_INSURANCE), 10);
    r.block(1000, None, vec![ins]);
    let (ka, _kb) = r.open(700);
    let a = addr(&ka);
    r.block(11_000, None, vec![]); // halt
    // resume 15% lower in one confirmed round (confirmRounds = 1 in the test market): a's equity is negative
    let out = r.block(1000, Some(P * 85 / 100 / 10_000_000 * 10_000_000), vec![]);
    assert!(out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })));
    let cover: i128 = out.events.iter().filter_map(|e| match e { Event::InsuranceCover { amount, .. } => Some(*amount), _ => None }).sum();
    assert_eq!(cover, 10_000_000, "the whole insurance balance is drawn before deleveraging");
    assert!(r.e.accounts[r.e.by_addr[&a] as usize].positions.is_empty());
}
