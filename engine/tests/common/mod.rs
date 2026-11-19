//! Shared rig of the round-2 tests (throwaway keys only; UNAUDITED prototype).
#![allow(dead_code)]
use k256::ecdsa::SigningKey;
use postern_perps_engine::genesis::Genesis;
use postern_perps_engine::hash::Addr;
use postern_perps_engine::invariants;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

pub const T0: u64 = 1_794_880_000_000;
/// BTC 67,250.0 and ETH 3,480.0 in price units.
pub const P: u64 = 6_725_000_000_000;
pub const PE: u64 = 348_000_000_000;
pub const TICK: u64 = 10_000_000;

pub fn tick(p: u64) -> u64 {
    p / TICK * TICK
}

pub struct Rig {
    pub e: Engine,
    pub ok: Vec<SigningKey>,
    pub h: u64,
    pub t: u64,
    pub dep: u64,
    pub fid: u64,
    pub log: Vec<Block>,
    pub roots: Vec<[u8; 32]>,
}

pub fn two_markets() -> Genesis {
    genesis_raw(7, vec![market(1, "BTC-USD"), market(2, "ETH-USD")])
}

impl Rig {
    pub fn new(g: Genesis) -> Rig {
        Rig { e: Engine::new(g.check().unwrap()), ok: oracle_keys(7), h: 0, t: T0, dep: 0, fid: 0, log: vec![], roots: vec![] }
    }
    pub fn one() -> Rig {
        Rig::new(genesis_raw(7, vec![market(1, "BTC-USD")]))
    }
    pub fn dep(&mut self, a: Addr, usdc: u64) -> Input {
        let i = Input::Deposit { index: self.dep, account: a, amount: usdc * 1_000_000 };
        self.dep += 1;
        i
    }
    pub fn forced(&mut self, a: Addr, kind: u8, arg: u64) -> Input {
        let i = Input::Forced { index: self.fid, account: a, kind, deadline: T0 / 1000 + 86_400, arg };
        self.fid += 1;
        i
    }
    /// One block `dt` ms later; `prices` empty = no oracle input. Invariants are checked after every block.
    pub fn block(&mut self, dt: u64, prices: &[(u32, u64)], mut rest: Vec<Input>) -> BlockOutcome {
        self.h += 1;
        self.t += dt;
        let mut inputs = vec![];
        if !prices.is_empty() {
            inputs.push(oracle_round(&self.e, &self.ok, self.t, prices));
        }
        inputs.append(&mut rest);
        let b = Block { height: self.h, time: self.t, inputs };
        let out = self.e.apply_block(&b).unwrap();
        if let Err(m) = invariants::check(&self.e) {
            panic!("invariant broken at height {}: {m}", self.h);
        }
        self.log.push(b);
        self.roots.push(out.state_root);
        out
    }
    pub fn ai(&self, a: &Addr) -> u32 {
        self.e.by_addr[a]
    }
    pub fn bal(&self, a: &Addr) -> i128 {
        self.e.accounts[self.ai(a) as usize].balance
    }
    pub fn size(&self, ai: u32, mid: u32) -> i64 {
        self.e.accounts[ai as usize].positions.get(&mid).map(|p| p.size).unwrap_or(0)
    }
    /// `maker` rests an ask at `price`, `taker` buys it with a market order, in one block with an oracle round.
    pub fn trade(&mut self, prices: &[(u32, u64)], mid: u32, maker: &SigningKey, taker: &SigningKey, price: u64, size: u64) -> BlockOutcome {
        let n = self.t + 3000;
        let ask = sign_action(&self.e, maker, place(addr(maker), n, mid, false, ORDER_LIMIT, TIF_GTC, price, size, false));
        let buy = sign_action(&self.e, taker, place(addr(taker), n, mid, true, ORDER_MARKET, TIF_IOC, price + 100 * TICK, size, false));
        let out = self.block(3000, prices, vec![ask, buy]);
        assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
        out
    }
}
