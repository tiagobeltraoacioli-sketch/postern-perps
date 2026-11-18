//! Agent 436 (round 2): independent property fuzzer of the perps engine. UNAUDITED review tooling.
//!
//! Independence: every input byte is built by the SPEC-only encoder of this crate's `lib.rs` (395, written from
//! docs/SPEC.md) and by the oracle encoder below (SPEC 3.2 + 394's README 4), signed here, and handed to the engine
//! ONLY as canonical bytes through its strict decoder. The properties are judged from this program's OWN ledger of
//! inputs and of the engine's emitted events, never from the engine's own counters or its `invariants::check`:
//!
//!  P1  I-1: positions rebuilt from Fill / MarketSettled events equal the engine's, per account and market;
//!      sum of sizes per market is zero; long OI <= oiCap.
//!  P2  I-2: 10^10 * sum(balances) + dust - sum(openNotional) == 10^10 * custody, custody = deposits this program
//!      saw accepted minus the amounts of the Withdrawal events it saw.
//!  P3  every Withdrawal event has a cause this program can name (an accepted signed Withdraw with the same account,
//!      recipient and gross - fee; a served forced withdraw to the account itself; the fee sweep to feeRecipient),
//!      ids are consecutive, and every accepted Withdraw produced exactly one record.
//!  P4  PROTOCOL_FEE balance == fees of all Fill events + flat withdrawal fees - sweeps (own ledger).
//!  P5  sum(escapeValue) <= custody + max(0, deficit - insurance equity), deficit and equity computed here.
//!  P6  replay: the stored canonical bytes, decoded and applied to a fresh engine, give the same state root at
//!      every height; the engine's re-encoding of the decoded inputs equals the bytes; logHash chain recomputed.
//!  P7  an identical action included again never succeeds; a high-s twin, a stranger's signature and an agent
//!      signing a main-key type never succeed.
//!  P8  no user account with positions only in active markets ends a block with E < MR (computed here from SPEC
//!      6.2) unless a Liquidated event for it was emitted in that block.
//!  P9  the engine never panics (overflow checks and debug assertions ON in this build) and never refuses a
//!      well-formed block.
//!  P10 no book is crossed after a block; INSURANCE and PROTOCOL_FEE balances are never negative.
//!  Soft counters (reported, not failures): BACKSTOP negative; insurance paying out while the index never moved
//!  (the R436-1 class); accounts still below maintenance after their liquidation step.
//!
//! Usage: fuzz436 <first_seed> <seeds> <blocks_per_seed> <threads> [minutes]
//! THROWAWAY keys only (derived from public integers).
use k256::ecdsa::SigningKey;
use perps_review_fuzz as sp;
use postern_perps_engine as pe;
use pe::engine::{system_address, Engine, Event, ACCT_BACKSTOP, ACCT_INSURANCE, ACCT_PROTOCOL_FEE};
use pe::oracle::Status;
use sp::Val;
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

const T0: u64 = 1_794_880_000_000;
const N10: i128 = 10_000_000_000;
const USERS: usize = 8;
const VAULT: [u8; 20] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0b, 0xa4, 0x17];

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo + 1) as u64) as i64
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

fn word(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// 394 README 4 / SPEC O-7: OracleObservation under the engine domain.
fn oracle_digest(dom: &sp::Domain, round: u64, time_ms: u64, prices: &[(u32, u64, u8)]) -> [u8; 32] {
    let tp = sp::keccak(b"OraclePrice(uint32 marketId,uint64 price,uint8 sources)");
    let to = sp::keccak(b"OracleObservation(uint64 round,uint64 timeMs,OraclePrice[] prices)OraclePrice(uint32 marketId,uint64 price,uint8 sources)");
    let mut arr = Vec::new();
    for (m, p, s) in prices {
        let mut e = tp.to_vec();
        e.extend_from_slice(&word(*m as u64));
        e.extend_from_slice(&word(*p));
        e.extend_from_slice(&word(*s as u64));
        arr.extend_from_slice(&sp::keccak(&e));
    }
    let mut e = to.to_vec();
    e.extend_from_slice(&word(round));
    e.extend_from_slice(&word(time_ms));
    e.extend_from_slice(&sp::keccak(&arr));
    let mut m = vec![0x19, 0x01];
    m.extend_from_slice(&dom.separator());
    m.extend_from_slice(&sp::keccak(&e));
    sp::keccak(&m)
}

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Any,
    MustFail,
}

#[derive(Clone)]
struct Meta {
    expect: Expect,
    why: &'static str,
    /// (account, recipient, gross) of a signed Withdraw
    withdraw: Option<([u8; 20], [u8; 20], u64)>,
    deposit: Option<([u8; 20], u64)>,
    payload: Vec<u8>,
    is_action: bool,
}

#[derive(Default)]
struct Stats {
    c: BTreeMap<&'static str, u64>,
    first: BTreeMap<&'static str, String>,
}
impl Stats {
    fn add(&mut self, k: &'static str, n: u64) {
        *self.c.entry(k).or_default() += n;
    }
    fn soft(&mut self, k: &'static str, msg: String) {
        *self.c.entry(k).or_default() += 1;
        self.first.entry(k).or_insert(msg);
    }
    fn merge(&mut self, o: Stats) {
        for (k, v) in o.c {
            *self.c.entry(k).or_default() += v;
        }
        for (k, v) in o.first {
            self.first.entry(k).or_insert(v);
        }
    }
}

struct World {
    seed: u64,
    rng: Rng,
    e: Engine,
    dom: sp::Domain,
    users: Vec<SigningKey>,
    agents: Vec<SigningKey>,
    stranger: SigningKey,
    okeys: Vec<SigningKey>,
    t: u64,
    h: u64,
    next_dep: u64,
    next_forced: u64,
    forced: BTreeMap<u64, ([u8; 20], u8, u64)>,
    px: [u64; 2],
    mode: u64,
    last_round: u64,
    nonce_ctr: u64,
    custody: i128,
    fee_ledger: i128,
    pos: BTreeMap<(u32, u32), i64>,
    wd_next: u64,
    pool: Vec<Vec<u8>>,
    log: Vec<(u64, u64, Vec<u8>)>,
    roots: Vec<[u8; 32]>,
    log_hash: [u8; 32],
    ins_prev: i128,
    stats: Stats,
}

fn addr(k: &SigningKey) -> [u8; 20] {
    sp::address_of(k.verifying_key())
}

fn tier_mr(n: i128, tiers: &[pe::genesis::Tier]) -> i128 {
    if n <= 0 {
        return 0;
    }
    let mut acc: i128 = 0;
    for (i, t) in tiers.iter().enumerate() {
        let lo = t.floor as i128 * N10;
        if n <= lo {
            break;
        }
        let hi = tiers.get(i + 1).map(|x| x.floor as i128 * N10).unwrap_or(i128::MAX);
        acc += (n.min(hi) - lo) * t.mmf_ppm as i128;
    }
    (acc + 999_999) / 1_000_000
}

impl World {
    fn new(seed: u64) -> World {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03) ^ 0x436);
        let n_or = 7;
        let mut m1 = pe::testkit::market(1, "BTC-USD");
        let mut m2 = pe::testkit::market(2, "ETH-USD");
        if rng.chance(30) {
            m1.oi_cap = 2_000_000_000; // 20 BTC: the OI cap binds
            m2.oi_cap = 20_000_000_000;
        }
        if rng.chance(30) {
            m1.oracle.confirm_rounds = 3;
            m2.oracle.confirm_rounds = 2;
        }
        let e = Engine::new(pe::testkit::genesis(n_or, vec![m1, m2]));
        let dom = sp::Domain::production_like(VAULT);
        assert_eq!(dom.separator(), e.domain_sep, "SPEC 2.1 domain separator: this program's equals the engine's");
        let log_hash = e.log_hash;
        let mode = rng.below(3);
        World {
            seed,
            users: (0..USERS as u32).map(|i| pe::testkit::key(43_600 + i)).collect(),
            agents: (0..USERS as u32).map(|i| pe::testkit::key(43_700 + i)).collect(),
            stranger: pe::testkit::key(43_999),
            okeys: pe::testkit::oracle_keys(n_or),
            rng,
            e,
            dom,
            t: T0,
            h: 0,
            next_dep: 0,
            next_forced: 0,
            forced: BTreeMap::new(),
            px: [6_725_000_000_000, 348_000_000_000],
            mode,
            last_round: 0,
            nonce_ctr: 0,
            custody: 0,
            fee_ledger: 0,
            pos: BTreeMap::new(),
            wd_next: 0,
            pool: Vec::new(),
            log: Vec::new(),
            roots: Vec::new(),
            log_hash,
            ins_prev: 0,
            stats: Stats::default(),
        }
    }

    fn nonce(&mut self) -> u64 {
        self.nonce_ctr += 1;
        // mostly inside the window around the block time, sometimes out of it
        match self.rng.below(40) {
            0 => self.t.saturating_sub(121_000 + self.nonce_ctr % 1000),
            1 => self.t + 30_001 + self.nonce_ctr % 1000,
            _ => self.t - (self.rng.below(100_000)) + self.rng.below(25_000) + (self.nonce_ctr % 7),
        }
    }

    fn sign_action(&self, a: &sp::Action, k: &SigningKey) -> Vec<u8> {
        let d = a.digest(&self.dom);
        sp::input(0x04, &a.wire_payload(&sp::sign(k, &d)))
    }

    fn price_near(&mut self, mid: u32) -> u64 {
        let m = &self.e.markets[&mid];
        let tick = m.p.tick;
        let base = if m.mark > 0 { m.mark } else { self.px[(mid - 1) as usize] };
        let p = if self.rng.chance(8) {
            // far from the market: 0.3x to 3x
            (base as u128 * (30 + self.rng.below(270)) as u128 / 100) as u64
        } else {
            (base as i64 + self.rng.range(-60, 60) * tick as i64).max(tick as i64) as u64
        };
        (p / tick).max(1) * tick
    }

    fn gen_action(&mut self) -> (Vec<u8>, Meta) {
        let ui = self.rng.below(USERS as u64) as usize;
        let key = self.users[ui].clone();
        let acct = addr(&key);
        let mid = 1 + self.rng.below(2) as u32;
        let mut meta = Meta { expect: Expect::Any, why: "", withdraw: None, deposit: None, payload: Vec::new(), is_action: true };
        // who signs: the account, its agent slot (maybe approved), or a stranger
        let roll = self.rng.below(100);
        let (signer, by_agent, by_stranger) = if roll < 80 { (key.clone(), false, false) } else if roll < 97 { (self.agents[ui].clone(), true, false) } else { (self.stranger.clone(), false, true) };
        let n = self.nonce();
        let kind = self.rng.below(100);
        let a = if kind < 62 {
            let ty = [0u8, 0, 0, 0, 1, 1, 2, 3, 4, 5][self.rng.below(10) as usize];
            let tif = if ty == 1 || ty == 2 || ty == 4 { 1 } else { self.rng.below(3) as u8 };
            let tif = if self.rng.chance(2) { 3 } else { tif };
            let price = self.price_near(mid);
            let step = self.e.markets[&mid].p.step;
            let big = if self.rng.chance(10) { 5000 } else { 400 };
            let off_step = if self.rng.chance(2) { 1 } else { 0 };
            let size = (1 + self.rng.below(big)) * step + off_step;
            let trig = if ty >= 2 { self.price_near(mid) } else if self.rng.chance(1) { 5 } else { 0 };
            let mut coid = [0u8; 32];
            coid[31] = self.rng.below(256) as u8;
            sp::Action {
                ty: "PlaceOrder",
                fields: vec![Val::Address(acct), Val::U64(n), Val::U32(mid), Val::Bool(self.rng.chance(50)), Val::U8(ty), Val::U8(tif), Val::U64(price), Val::U64(size), Val::Bool(self.rng.chance(15)), Val::U64(trig), Val::B32(coid)],
            }
        } else if kind < 72 {
            let ai = self.e.by_addr.get(&acct).copied();
            let ids: Vec<u64> = ai.map(|i| self.e.accounts[i as usize].orders.iter().copied().collect()).unwrap_or_default();
            let (oid, m) = if ids.is_empty() || self.rng.chance(10) { (self.rng.below(self.e.next_order_id + 3), mid) } else {
                let id = ids[self.rng.below(ids.len() as u64) as usize];
                (id, self.e.orders[&id].market)
            };
            sp::Action { ty: "CancelOrder", fields: vec![Val::Address(acct), Val::U64(n), Val::U32(m), Val::U64(oid)] }
        } else if kind < 76 {
            sp::Action { ty: "CancelAll", fields: vec![Val::Address(acct), Val::U64(n), Val::U32(self.rng.below(4) as u32)] }
        } else if kind < 82 {
            sp::Action { ty: "UpdateLeverage", fields: vec![Val::Address(acct), Val::U64(n), Val::U32(mid), Val::U32(self.rng.below(56) as u32), Val::Bool(self.rng.chance(5))] }
        } else if kind < 84 {
            sp::Action { ty: "UpdateMargin", fields: vec![Val::Address(acct), Val::U64(n), Val::U32(mid), Val::U64(self.rng.below(1_000_000_000)), Val::Bool(self.rng.chance(50))] }
        } else if kind < 91 {
            let bal = self.e.by_addr.get(&acct).map(|i| self.e.accounts[*i as usize].balance).unwrap_or(0).max(0) as u64;
            let amount = match self.rng.below(5) {
                0 => bal,
                1 => self.rng.below(2_000_000),
                2 => bal.saturating_add(1 + self.rng.below(1000)),
                _ => self.rng.below(bal.max(1)),
            };
            let recipient = match self.rng.below(10) {
                0 => [0u8; 20],
                1 => VAULT,
                2 => addr(&self.users[(ui + 1) % USERS]),
                _ => acct,
            };
            meta.withdraw = Some((acct, recipient, amount));
            sp::Action { ty: "Withdraw", fields: vec![Val::Address(acct), Val::U64(n), Val::U64(amount), Val::Address(recipient)] }
        } else if kind < 97 {
            let agent = if self.rng.chance(85) { addr(&self.agents[ui]) } else if self.rng.chance(50) { addr(&self.users[(ui + 1) % USERS]) } else { addr(&self.agents[(ui + 1) % USERS]) };
            let exp = match self.rng.below(6) {
                0 => self.t,
                1 => self.t + 181 * 86_400_000,
                2 => self.t + 1 + self.rng.below(5_000),
                _ => self.t + 60_000 + self.rng.below(86_400_000),
            };
            let name = if self.rng.chance(5) { "x".repeat(33) } else { format!("bot{}", self.rng.below(9)) };
            sp::Action { ty: "ApproveAgent", fields: vec![Val::Address(acct), Val::U64(n), Val::Address(agent), Val::Str(name), Val::U64(exp)] }
        } else {
            sp::Action { ty: "RevokeAgent", fields: vec![Val::Address(acct), Val::U64(n), Val::Address(addr(&self.agents[ui]))] }
        };
        if by_stranger {
            meta.expect = Expect::MustFail;
            meta.why = "signed by a key that is neither the account nor ever an agent";
        }
        if by_agent && matches!(a.ty, "Withdraw" | "ApproveAgent" | "RevokeAgent") {
            meta.expect = Expect::MustFail;
            meta.why = "main-key type signed by the agent key";
        }
        let mut bytes = self.sign_action(&a, &signer);
        if self.rng.chance(2) {
            // high-s twin of the same signature (S-2.2)
            let d = a.digest(&self.dom);
            let sig = sp::sign(&signer, &d);
            bytes = sp::input(0x04, &a.wire_payload(&sp::high_s_twin(&sig)));
            meta.expect = Expect::MustFail;
            meta.why = "high-s twin signature";
        }
        meta.payload = bytes.clone();
        (bytes, meta)
    }

    fn oracle_input(&mut self) -> Option<Vec<u8>> {
        let round = self.t / 3000;
        if round <= self.last_round {
            return None;
        }
        self.last_round = round;
        // price process
        for i in 0..2 {
            let mv: i64 = match self.mode {
                0 => 0,
                1 => self.rng.range(-300, 300),
                _ => {
                    if self.rng.chance(4) {
                        self.rng.range(-1500, 1500)
                    } else {
                        self.rng.range(-450, 450)
                    }
                }
            };
            let p = self.px[i] as i128 * (10_000 + mv as i128) / 10_000;
            self.px[i] = ((p as u64) / 10_000_000).clamp(100, 50_000_000) * 10_000_000;
        }
        let time_ms = round * 3000;
        let n = self.okeys.len();
        let count = 4 + self.rng.below(4) as usize; // quorum 5 of 7: 4 = below quorum sometimes
        let mut signers: Vec<usize> = (0..n).collect();
        while signers.len() > count {
            let i = self.rng.below(signers.len() as u64) as usize;
            signers.remove(i);
        }
        let skip_eth = self.mode == 2 && self.rng.chance(6);
        let mut p = round.to_be_bytes().to_vec();
        p.push(signers.len() as u8);
        for s in signers {
            let mut prices: Vec<(u32, u64, u8)> = Vec::new();
            for (i, mid) in [1u32, 2].iter().enumerate() {
                if *mid == 2 && skip_eth {
                    continue;
                }
                let jitter = if self.mode == 0 { 0 } else if self.rng.chance(3) { self.rng.range(-300, 300) } else { self.rng.range(-20, 20) };
                let price = (self.px[i] as i128 * (10_000 + jitter as i128) / 10_000).max(1) as u64;
                let sources = if self.mode == 2 && self.rng.chance(2) { 2 } else { 5 };
                prices.push((*mid, price, sources));
            }
            let d = oracle_digest(&self.dom, round, time_ms, &prices);
            let sig = sp::sign(&self.okeys[s], &d);
            p.push(s as u8);
            p.extend_from_slice(&time_ms.to_be_bytes());
            p.extend_from_slice(&(prices.len() as u16).to_be_bytes());
            for (m, price, src) in &prices {
                p.extend_from_slice(&m.to_be_bytes());
                p.extend_from_slice(&price.to_be_bytes());
                p.push(*src);
            }
            p.extend_from_slice(&sig);
        }
        Some(sp::input(0x01, &p))
    }

    fn deposit(&mut self, account: [u8; 20], amount: u64, metas: &mut Vec<Meta>, inputs: &mut Vec<Vec<u8>>) {
        let b = sp::input(0x02, &sp::deposit_payload(self.next_dep, account, amount));
        self.next_dep += 1;
        metas.push(Meta { expect: Expect::Any, why: "", withdraw: None, deposit: Some((account, amount)), payload: b.clone(), is_action: false });
        inputs.push(b);
    }

    fn step(&mut self) -> Result<(), String> {
        let dt = match self.rng.below(200) {
            0 => 86_400_000,
            1..=3 => 3_600_000,
            4..=9 => 12_000 + self.rng.below(60_000),
            10..=60 => 1_000 + self.rng.below(2_500),
            _ => 200 + self.rng.below(600),
        };
        let dt = if self.mode != 2 && dt > 9_000 && dt < 3_600_000 { 3000 } else { dt };
        self.t += dt;
        self.h += 1;
        let mut inputs: Vec<Vec<u8>> = Vec::new();
        let mut metas: Vec<Meta> = Vec::new();
        if !(self.mode == 2 && self.rng.chance(5)) {
            if let Some(o) = self.oracle_input() {
                metas.push(Meta { expect: Expect::Any, why: "", withdraw: None, deposit: None, payload: o.clone(), is_action: false });
                inputs.push(o);
            }
        }
        // deposits
        if self.h == 1 {
            let (ins, bs) = (self.rng.below(3), self.rng.below(3));
            if ins > 0 {
                self.deposit(system_address(ACCT_INSURANCE), 60_000_000_000 / ins, &mut metas, &mut inputs);
            }
            if bs > 0 {
                self.deposit(system_address(ACCT_BACKSTOP), 25_000_000_000 / bs, &mut metas, &mut inputs);
            }
            for i in 0..USERS {
                if self.rng.chance(85) {
                    let amt = 1_000_000 * (500 + self.rng.below(200_000));
                    self.deposit(addr(&self.users[i].clone()), amt, &mut metas, &mut inputs);
                }
            }
        } else if self.rng.chance(6) {
            let who = match self.rng.below(30) {
                0 => addr(&self.agents[self.rng.below(USERS as u64) as usize].clone()),
                1 => system_address(self.rng.below(3) as u32),
                _ => addr(&self.users[self.rng.below(USERS as u64) as usize].clone()),
            };
            let amt = 1 + self.rng.below(50_000_000_000);
            self.deposit(who, amt, &mut metas, &mut inputs);
            if self.rng.chance(3) {
                // an out-of-order deposit index: must fail and change nothing
                let b = sp::input(0x02, &sp::deposit_payload(self.next_dep + 1, who, amt));
                metas.push(Meta { expect: Expect::MustFail, why: "deposit index not consecutive", withdraw: None, deposit: None, payload: b.clone(), is_action: false });
                inputs.push(b);
            }
        }
        // forced requests
        if self.rng.chance(4) {
            let who = if self.rng.chance(97) { addr(&self.users[self.rng.below(USERS as u64) as usize].clone()) } else { system_address(1) };
            let kind = [0u8, 1, 1, 2, 2, 2, 3][self.rng.below(7) as usize];
            let arg = if self.rng.chance(50) { 0 } else { self.rng.below(100_000_000_000) };
            let mut p = self.next_forced.to_be_bytes().to_vec();
            p.extend_from_slice(&who);
            p.push(kind);
            p.extend_from_slice(&(self.t / 1000 + 86_400).to_be_bytes());
            p.extend_from_slice(&arg.to_be_bytes());
            self.forced.insert(self.next_forced, (who, kind, arg));
            self.next_forced += 1;
            let b = sp::input(0x03, &p);
            metas.push(Meta { expect: Expect::Any, why: "", withdraw: None, deposit: None, payload: b.clone(), is_action: false });
            inputs.push(b);
        }
        // actions
        let n_act = if self.h < 2 { 0 } else { self.rng.below(14) };
        for _ in 0..n_act {
            if !self.pool.is_empty() && self.rng.chance(4) {
                let b = self.pool[self.rng.below(self.pool.len() as u64) as usize].clone();
                metas.push(Meta { expect: Expect::MustFail, why: "an action already included once", withdraw: None, deposit: None, payload: b.clone(), is_action: true });
                inputs.push(b);
                continue;
            }
            let (b, m) = self.gen_action();
            // The engine's DECODER refuses an ApproveAgent name over 32 bytes, which refuses the whole block (R436-4:
            // SPEC 3.2 says an invalid input is a failed outcome, never a refused block). Such an input cannot be
            // put in a block here; it is checked alone and counted.
            if pe::codec::decode_inputs(&sp::inputs_encoded(&[b.clone()])).is_err() {
                self.stats.soft("single_action_refused_by_the_block_decoder(R436-4)", format!("seed {} block {}", self.seed, self.h));
                continue;
            }
            inputs.push(b);
            metas.push(m);
        }
        let enc = sp::inputs_encoded(&inputs);
        // the engine sees only bytes
        let decoded = pe::codec::decode_inputs(&enc).map_err(|e| format!("P6: engine decoder refused SPEC-encoded inputs: {}", e.0))?;
        if pe::codec::encode_inputs(&decoded) != enc {
            return Err("P6: engine re-encoding differs from the SPEC encoder's bytes".into());
        }
        let blk = pe::Block { height: self.h, time: self.t, inputs: decoded };
        let day_boundary = (self.t / 86_400_000) > ((self.t - dt) / 86_400_000);
        let e = &mut self.e;
        let out = catch_unwind(AssertUnwindSafe(|| e.apply_block(&blk))).map_err(|p| {
            let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            format!("P9: engine PANICKED applying block {}: {msg}", blk.height)
        })?;
        let out = out.map_err(|e| format!("P9: well-formed block refused: {e:?}"))?;
        self.log_hash = sp::log_hash(&self.log_hash, self.h, self.t, &enc);
        if self.log_hash != out.log_hash {
            return Err("P6: logHash chain (SPEC 3.1) differs".into());
        }
        if out.outcomes.len() != metas.len() {
            return Err("outcome count differs from input count".into());
        }
        let fee = self.e.g.g.withdrawal_fee as i128;
        // own ledger from inputs + outcomes
        let mut expected_wd: Vec<([u8; 20], [u8; 20], Option<u64>)> = Vec::new();
        for (m, o) in metas.iter().zip(&out.outcomes) {
            if m.expect == Expect::MustFail && o.is_ok() {
                return Err(format!("P7: input that must fail was accepted ({}) at block {}", m.why, self.h));
            }
            if let (Some((acct, amount)), true) = (m.deposit, o.is_ok()) {
                self.custody += amount as i128;
                if acct == system_address(ACCT_PROTOCOL_FEE) {
                    self.fee_ledger += amount as i128;
                }
            }
            if m.deposit.is_some() && m.expect == Expect::Any && o.is_err() {
                return Err(format!("consecutive deposit refused: {o:?}"));
            }
            if let (Some((a, r, gross)), true) = (m.withdraw, o.is_ok()) {
                if r == [0u8; 20] {
                    return Err("withdrawal to the zero address accepted".into());
                }
                expected_wd.push((a, r, Some((gross as i128 - fee) as u64)));
                if r == VAULT {
                    self.stats.add("withdraw_to_vault_address_accepted(R395-22)", 1);
                }
            }
            if m.is_action {
                match o {
                    Ok(()) => {
                        self.stats.add("actions_ok", 1);
                        if m.expect == Expect::Any && self.pool.len() < 400 {
                            self.pool.push(m.payload.clone());
                        }
                    }
                    Err(er) => {
                        self.stats.add("actions_err", 1);
                        use pe::Error::*;
                        if !matches!(er, BadSignature | UnknownAccount | NonceWindow | NonceUsed | AgentNotAllowed) && m.expect == Expect::Any && self.pool.len() < 400 {
                            self.pool.push(m.payload.clone());
                        }
                    }
                }
            }
        }
        // events
        let mut liquidated: Vec<u32> = Vec::new();
        let mut got_wd: Vec<pe::engine::WithdrawalRecord> = Vec::new();
        let mut cover: i128 = 0;
        for ev in &out.events {
            match ev {
                Event::Fill { market, taker, maker, taker_is_buy, size, taker_fee, maker_fee, kind, .. } => {
                    let d = if *taker_is_buy { *size as i64 } else { -(*size as i64) };
                    *self.pos.entry((*taker, *market)).or_default() += d;
                    *self.pos.entry((*maker, *market)).or_default() -= d;
                    self.fee_ledger += *taker_fee + *maker_fee;
                    if *taker_fee < 0 || *maker_fee < 0 {
                        return Err("negative fee in a Fill event".into());
                    }
                    self.stats.add(
                        match kind {
                            pe::engine::FillKind::Book => "fills_book",
                            pe::engine::FillKind::Liquidation => "fills_liquidation",
                            pe::engine::FillKind::Backstop => "fills_backstop",
                            pe::engine::FillKind::Adl => "fills_adl",
                        },
                        1,
                    );
                }
                Event::MarketSettled { market, .. } => {
                    self.pos.retain(|(_, m), _| m != market);
                    self.stats.add("markets_settled_x3", 1);
                }
                Event::Withdrawal(w) => got_wd.push(w.clone()),
                Event::Liquidated { account } => {
                    liquidated.push(*account);
                    self.stats.add("liquidations", 1);
                }
                Event::ForcedServed { index } => {
                    self.stats.add("forced_served", 1);
                    if let Some((who, 2, arg)) = self.forced.get(index).copied() {
                        expected_wd.push((who, who, if arg > 0 { Some((arg as i128 - fee) as u64) } else { None }));
                    }
                }
                Event::ForcedRejected { .. } => self.stats.add("forced_rejected", 1),
                Event::ForcedPending { .. } => self.stats.add("forced_pending_events", 1),
                Event::Funding { .. } => self.stats.add("funding_settlements", 1),
                Event::Halt { .. } => self.stats.add("halts", 1),
                Event::InsuranceCover { amount, .. } => {
                    cover += *amount;
                    self.stats.add("insurance_covers", 1);
                }
                Event::BadDebt { .. } => self.stats.add("bad_debt_events", 1),
                Event::Triggered { .. } => self.stats.add("triggers_fired", 1),
                _ => {}
            }
        }
        // P3 withdrawals
        for w in &got_wd {
            if w.id != self.wd_next {
                return Err(format!("P3: withdrawal id {} not consecutive (expected {})", w.id, self.wd_next));
            }
            self.wd_next += 1;
            self.custody -= w.amount as i128;
            if w.height != self.h {
                return Err("P3: withdrawal record height differs from the block".into());
            }
            let sweep = w.account == system_address(ACCT_PROTOCOL_FEE);
            if sweep {
                if w.recipient != self.e.g.fee_recipient || !day_boundary {
                    return Err(format!("P3: fee sweep to a wrong recipient or outside a day boundary at block {}", self.h));
                }
                self.fee_ledger -= w.amount as i128;
                self.stats.add("fee_sweeps", 1);
                continue;
            }
            self.fee_ledger += fee;
            let i = expected_wd.iter().position(|(a, r, amt)| *a == w.account && *r == w.recipient && amt.is_none_or(|x| x == w.amount));
            match i {
                Some(i) => {
                    expected_wd.remove(i);
                }
                None => return Err(format!("P3: withdrawal record {} ({} units to {}) has no cause this program can name, block {}", w.id, w.amount, hex::encode(w.recipient), self.h)),
            }
            self.stats.add("withdrawals", 1);
        }
        if !expected_wd.is_empty() {
            return Err(format!("P3: {} accepted withdrawals produced no record at block {}", expected_wd.len(), self.h));
        }
        // P1
        self.pos.retain(|_, s| *s != 0);
        let mut sums: BTreeMap<u32, i128> = BTreeMap::new();
        let mut longs: BTreeMap<u32, i128> = BTreeMap::new();
        let mut eng_count = 0;
        for (ai, a) in self.e.accounts.iter().enumerate() {
            for (mid, p) in &a.positions {
                eng_count += 1;
                let mine = self.pos.get(&(ai as u32, *mid)).copied().unwrap_or(0);
                if mine != p.size {
                    return Err(format!("P1: account {ai} market {mid}: engine size {} != event ledger {mine}, block {}", p.size, self.h));
                }
                *sums.entry(*mid).or_default() += p.size as i128;
                if p.size > 0 {
                    *longs.entry(*mid).or_default() += p.size as i128;
                }
            }
        }
        if eng_count != self.pos.len() {
            return Err(format!("P1: the event ledger holds {} positions, the engine {eng_count}", self.pos.len()));
        }
        for (mid, s) in &sums {
            if *s != 0 {
                return Err(format!("P1 (I-1): market {mid} sizes sum to {s}"));
            }
            if longs.get(mid).copied().unwrap_or(0) > self.e.markets[mid].p.oi_cap as i128 {
                return Err(format!("P1: long OI above oiCap in market {mid}, block {}", self.h));
            }
        }
        // P2
        let bal: i128 = self.e.accounts.iter().map(|a| a.balance).sum();
        let on: i128 = self.e.accounts.iter().flat_map(|a| a.positions.values()).map(|p| p.open_notional).sum();
        if N10 * bal + self.e.dust - on != N10 * self.custody {
            return Err(format!("P2 (I-2): 1e10*balances {} + dust {} - openNotional {} != 1e10*custody {} at block {}", N10 * bal, self.e.dust, on, N10 * self.custody, self.h));
        }
        if self.e.dust < 0 {
            return Err("dust negative".into());
        }
        // P4
        if self.e.accounts[ACCT_PROTOCOL_FEE as usize].balance != self.fee_ledger {
            return Err(format!("P4: PROTOCOL_FEE balance {} != fee ledger {} at block {}", self.e.accounts[ACCT_PROTOCOL_FEE as usize].balance, self.fee_ledger, self.h));
        }
        // P5, P8 with own E and MR
        let mut deficit: i128 = 0;
        let mut ins_e: i128 = 0;
        for (ai, a) in self.e.accounts.iter().enumerate() {
            let mut eq = a.balance * N10;
            let mut mr: i128 = 0;
            let mut all_active = true;
            for (mid, p) in &a.positions {
                let m = &self.e.markets[mid];
                eq += p.size as i128 * m.mark as i128 - p.open_notional;
                mr += tier_mr((p.size as i128).abs() * m.mark as i128, &m.p.tiers);
                all_active &= m.oracle.status == Status::Active && m.mark > 0;
            }
            if eq < 0 {
                deficit += (-eq + N10 - 1) / N10;
            }
            if ai as u32 == ACCT_INSURANCE {
                ins_e = eq.max(0) / N10;
            }
            if ai >= 3 && !a.positions.is_empty() && all_active && eq < mr {
                if !liquidated.contains(&(ai as u32)) {
                    return Err(format!("P8: account {ai} ends block {} with E {eq} < MR {mr} and no liquidation step", self.h));
                }
                self.stats.soft("below_maintenance_after_own_liquidation", format!("seed {} block {} account {ai} E {eq} MR {mr}", self.seed, self.h));
            }
        }
        let esc: i128 = self.e.escape_values().iter().map(|x| *x as i128).sum();
        let allowance = (deficit - ins_e).max(0);
        if esc > self.custody + allowance {
            return Err(format!("P5: sum(escape) {esc} > custody {} + uncovered deficit {allowance} at block {}", self.custody, self.h));
        }
        if allowance > 0 {
            self.stats.add("blocks_with_escape_haircut", 1);
        }
        // P10
        for (mid, m) in &self.e.markets {
            if let (Some(b), Some(a)) = (m.book.best_bid(), m.book.best_ask()) {
                if b >= a {
                    return Err(format!("P10: book of market {mid} crossed after block {}", self.h));
                }
            }
        }
        let ins = self.e.accounts[ACCT_INSURANCE as usize].balance;
        if ins < 0 || self.e.accounts[ACCT_PROTOCOL_FEE as usize].balance < 0 {
            return Err(format!("P10: INSURANCE or PROTOCOL_FEE negative at block {}", self.h));
        }
        if self.e.accounts[ACCT_BACKSTOP as usize].balance < 0 {
            self.stats.soft("backstop_balance_negative", format!("seed {} block {} balance {}", self.seed, self.h, self.e.accounts[ACCT_BACKSTOP as usize].balance));
        }
        if self.mode == 0 && cover > 0 {
            self.stats.soft("insurance_paid_while_index_constant(R436-1 class)", format!("seed {} block {} cover {} USDC units", self.seed, self.h, cover));
            self.stats.add("insurance_paid_while_index_constant_units", cover as u64);
        }
        // The engine's OWN invariant check, as a soft counter only (this program's verdicts never depend on it):
        // a state this fuzzer reaches and the engine's own check rejects is worth a look.
        if let Err(m) = pe::invariants::check(&self.e) {
            self.stats.soft("engine_own_invariants_check_fails", format!("seed {} block {}: {m}", self.seed, self.h));
        }
        self.ins_prev = ins;
        self.roots.push(out.state_root);
        self.log.push((self.h, self.t, enc));
        Ok(())
    }

    fn replay(&self) -> Result<(), String> {
        let mut m1 = self.e.g.g.clone();
        m1.markets = self.e.g.g.markets.clone();
        let mut e2 = Engine::new(m1.check().map_err(|e| format!("genesis re-check: {e}"))?);
        for (i, (h, t, enc)) in self.log.iter().enumerate() {
            let inputs = pe::codec::decode_inputs(enc).map_err(|e| format!("P6 replay decode: {}", e.0))?;
            let out = e2.apply_block(&pe::Block { height: *h, time: *t, inputs }).map_err(|e| format!("P6 replay block {h}: {e:?}"))?;
            if out.state_root != self.roots[i] {
                return Err(format!("P6 (I-5): replay state root differs at height {h}"));
            }
        }
        Ok(())
    }
}

fn run_seed(seed: u64, blocks: u64) -> (Stats, Option<String>) {
    let mut w = World::new(seed);
    for _ in 0..blocks {
        if let Err(e) = w.step() {
            return (w.stats, Some(format!("seed {seed} (mode {}): {e}", w.mode)));
        }
    }
    let r = w.replay().err().map(|e| format!("seed {seed}: {e}"));
    w.stats.add("blocks", blocks);
    w.stats.add("seeds", 1);
    (w.stats, r)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let get = |i: usize, d: u64| a.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let (first, seeds, blocks, threads, minutes) = (get(1, 1), get(2, 20), get(3, 400), get(4, 4).max(1), get(5, 0));
    let start = std::time::Instant::now();
    // quiet panics inside catch_unwind (they are reported as violations)
    std::panic::set_hook(Box::new(|_| {}));
    let next = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(first));
    let mut hs = Vec::new();
    for _ in 0..threads {
        let next = next.clone();
        hs.push(std::thread::spawn(move || {
            let mut st = Stats::default();
            let mut viol = Vec::new();
            loop {
                let s = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let timed_out = minutes > 0 && start.elapsed().as_secs() >= minutes * 60;
                if (minutes == 0 && s >= first + seeds) || timed_out {
                    break;
                }
                let (stats, v) = run_seed(s, blocks);
                st.merge(stats);
                if let Some(v) = v {
                    eprintln!("VIOLATION {v}");
                    viol.push(v);
                }
            }
            (st, viol)
        }));
    }
    let mut total = Stats::default();
    let mut viols = Vec::new();
    for h in hs {
        let (s, v) = h.join().expect("worker");
        total.merge(s);
        viols.extend(v);
    }
    println!("fuzz436: first seed {first}, {} s, {} threads, {blocks} blocks per seed", start.elapsed().as_secs(), threads);
    for (k, v) in &total.c {
        println!("  {k}: {v}");
    }
    for (k, v) in &total.first {
        println!("  first {k}: {v}");
    }
    println!("HARD VIOLATIONS: {}", viols.len());
    for v in viols.iter().take(40) {
        println!("  {v}");
    }
    std::process::exit(if viols.is_empty() { 0 } else { 1 });
}
