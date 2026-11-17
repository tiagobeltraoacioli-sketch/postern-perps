//! Property fuzzer: random traders, orders, cancels, withdrawals, deposits, leverage changes and oracle moves
//! (including halts and large drops that liquidate). After every block: the invariants of SPEC section 8. At the
//! end: the whole log re-encoded to canonical bytes, decoded and replayed on a fresh engine gives the same state
//! root at every height (I-5, codec canonicality).
use k256::ecdsa::SigningKey;
use postern_perps_engine::codec::{decode_inputs, encode_inputs};
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;
use postern_perps_engine::invariants;
use proptest::prelude::*;

const T0: u64 = 1_794_880_000_000;
const TRADERS: usize = 6;

#[derive(Clone, Debug)]
enum Op {
    Place { who: usize, mkt: u32, buy: bool, ty: u8, tif: u8, off: i64, steps: u64, ro: bool, trig_off: i64 },
    Cancel { who: usize, pick: usize },
    CancelAll { who: usize },
    Withdraw { who: usize, amount: u64 },
    Deposit { who: usize, amount: u64 },
    Leverage { who: usize, mkt: u32, lev: u32 },
    Block { dt: u64, oracle: bool, mv_bps: i64 },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..TRADERS, 1u32..=2, any::<bool>(), 0u8..=5, 0u8..=2, -40i64..40, 1u64..400, prop::bool::weighted(0.15), -30i64..30)
            .prop_map(|(who, mkt, buy, ty, tif, off, steps, ro, trig_off)| Op::Place { who, mkt, buy, ty, tif, off, steps, ro, trig_off }),
        1 => (0..TRADERS, 0usize..50).prop_map(|(who, pick)| Op::Cancel { who, pick }),
        1 => (0..TRADERS).prop_map(|who| Op::CancelAll { who }),
        1 => (0..TRADERS, 1u64..5_000_000_000).prop_map(|(who, amount)| Op::Withdraw { who, amount }),
        1 => (0..TRADERS, 1u64..20_000_000_000).prop_map(|(who, amount)| Op::Deposit { who, amount }),
        1 => (0..TRADERS, 1u32..=2, 1u32..=60).prop_map(|(who, mkt, lev)| Op::Leverage { who, mkt, lev }),
        3 => (prop_oneof![500u64..3000, 3000u64..20_000, Just(3_600_000u64)], prop::bool::weighted(0.85), -450i64..450)
            .prop_map(|(dt, oracle, mv_bps)| Op::Block { dt, oracle, mv_bps }),
    ]
}

struct Sim {
    e: Engine,
    ok: Vec<SigningKey>,
    keys: Vec<SigningKey>,
    t: u64,
    h: u64,
    dep: u64,
    px: [u64; 3],
    pending: Vec<Input>,
    deposits: Vec<Input>,
    nonce: u64,
    log: Vec<Block>,
    roots: Vec<[u8; 32]>,
}

fn round_tick(p: u64, tick: u64) -> u64 {
    (p / tick).max(1) * tick
}

impl Sim {
    fn new() -> Sim {
        let mut m2 = market(2, "ETH-USD");
        m2.tick = 1_000_000;
        m2.step = 1_000_000;
        m2.min_size = 1_000_000;
        m2.max_size = 100_000_000_000;
        m2.oi_cap = 2_000_000_000_000;
        let e = Engine::new(genesis(5, vec![market(1, "BTC-USD"), m2]));
        Sim { e, ok: oracle_keys(5), keys: (0..TRADERS as u32).map(|i| key(100 + i)).collect(), t: T0, h: 0, dep: 0, px: [0, 6_725_000_000_000, 348_000_000_000], pending: vec![], deposits: vec![], nonce: 0, log: vec![], roots: vec![] }
    }
    fn next_nonce(&mut self) -> u64 {
        self.nonce += 1;
        self.t + (self.nonce % 1000)
    }
    fn flush(&mut self, dt: u64, oracle: bool) {
        self.h += 1;
        self.t += dt;
        let mut inputs = vec![];
        if oracle {
            inputs.push(oracle_round(&self.e, &self.ok, self.t, &[(1, self.px[1]), (2, self.px[2])]));
        }
        inputs.append(&mut self.deposits);
        inputs.append(&mut self.pending);
        let b = Block { height: self.h, time: self.t, inputs };
        let out = self.e.apply_block(&b).expect("well-formed block applies");
        if let Err(msg) = invariants::check(&self.e) {
            panic!("invariant broken at height {}: {msg}", self.h);
        }
        self.roots.push(out.state_root);
        self.log.push(b);
    }
    fn run(&mut self, ops: &[Op]) {
        for who in 0..TRADERS {
            let a = addr(&self.keys[who]);
            self.deposits.push(Input::Deposit { index: self.dep, account: a, amount: 50_000_000_000 });
            self.dep += 1;
        }
        self.flush(1000, true);
        for op in ops {
            match op.clone() {
                Op::Block { dt, oracle, mv_bps } => {
                    for m in 1..=2 {
                        let tick = self.e.markets[&(m as u32)].p.tick;
                        let p = (self.px[m] as i128 * (10_000 + mv_bps as i128) / 10_000) as u64;
                        self.px[m] = round_tick(p, tick);
                    }
                    self.flush(dt, oracle);
                }
                Op::Place { who, mkt, buy, ty, tif, off, steps, ro, trig_off } => {
                    let m = &self.e.markets[&mkt].p;
                    let (tick, step) = (m.tick, m.step);
                    let base = self.px[mkt as usize] as i64;
                    let price = round_tick((base + off * tick as i64 * 10).max(tick as i64) as u64, tick);
                    let is_mkt = matches!(ty, 1 | 2 | 4);
                    let tif = if is_mkt { TIF_IOC } else if ty >= 2 && tif == TIF_POST_ONLY { TIF_GTC } else { tif };
                    let trig = if ty >= 2 { round_tick((base + trig_off * tick as i64 * 10).max(tick as i64) as u64, tick) } else { 0 };
                    let k = self.keys[who].clone();
                    let a = addr(&k);
                    let n = self.next_nonce();
                    let mut act = place(a, n, mkt, buy, ty, tif, price, steps * step, ro);
                    if let Action::PlaceOrder(p) = &mut act {
                        p.trigger_price = trig;
                    }
                    self.pending.push(sign_action(&self.e, &k, act));
                }
                Op::Cancel { who, pick } => {
                    let k = self.keys[who].clone();
                    let a = addr(&k);
                    let Some(&ai) = self.e.by_addr.get(&a) else { continue };
                    let ids: Vec<u64> = self.e.accounts[ai as usize].orders.iter().copied().collect();
                    if ids.is_empty() {
                        continue;
                    }
                    let id = ids[pick % ids.len()];
                    let mkt = self.e.orders[&id].market;
                    let n = self.next_nonce();
                    self.pending.push(sign_action(&self.e, &k, Action::CancelOrder { account: a, nonce: n, market_id: mkt, order_id: id }));
                }
                Op::CancelAll { who } => {
                    let k = self.keys[who].clone();
                    let a = addr(&k);
                    let n = self.next_nonce();
                    self.pending.push(sign_action(&self.e, &k, Action::CancelAll { account: a, nonce: n, market_id: 0 }));
                }
                Op::Withdraw { who, amount } => {
                    let k = self.keys[who].clone();
                    let a = addr(&k);
                    let n = self.next_nonce();
                    self.pending.push(sign_action(&self.e, &k, Action::Withdraw { account: a, nonce: n, amount, recipient: a }));
                }
                Op::Deposit { who, amount } => {
                    let a = addr(&self.keys[who]);
                    self.deposits.push(Input::Deposit { index: self.dep, account: a, amount });
                    self.dep += 1;
                }
                Op::Leverage { who, mkt, lev } => {
                    let k = self.keys[who].clone();
                    let a = addr(&k);
                    let n = self.next_nonce();
                    self.pending.push(sign_action(&self.e, &k, Action::UpdateLeverage { account: a, nonce: n, market_id: mkt, leverage: lev, isolated: false }));
                }
            }
        }
        self.flush(1000, true);
    }
}

fn replay_matches(sim: &Sim) {
    let mut fresh = Engine::new(sim.e.g.clone());
    for (b, root) in sim.log.iter().zip(&sim.roots) {
        let bytes = encode_inputs(&b.inputs);
        let decoded = decode_inputs(&bytes).expect("canonical bytes decode");
        let out = fresh.apply_block(&Block { height: b.height, time: b.time, inputs: decoded }).unwrap();
        assert_eq!(&out.state_root, root, "replay diverged at height {}", b.height);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64), max_shrink_iters: 2000, .. ProptestConfig::default() })]
    #[test]
    fn invariants_and_replay(ops in prop::collection::vec(op(), 1..250)) {
        let mut sim = Sim::new();
        sim.run(&ops);
        replay_matches(&sim);
    }
}

/// A deterministic heavy case: large adverse moves so that liquidations, backstop and ADL paths run.
#[test]
fn crash_and_recover_sequence() {
    let mut ops = vec![];
    for i in 0..300u64 {
        let who = (i % TRADERS as u64) as usize;
        ops.push(Op::Place { who, mkt: 1, buy: i % 2 == 0, ty: 0, tif: 0, off: (i as i64 % 7) - 3, steps: 50 + i % 300, ro: false, trig_off: 0 });
        ops.push(Op::Place { who: (who + 1) % TRADERS, mkt: 1, buy: i % 2 == 1, ty: 1, tif: 1, off: if i % 2 == 1 { 30 } else { -30 }, steps: 40 + i % 200, ro: false, trig_off: 0 });
        if i % 10 == 9 {
            ops.push(Op::Block { dt: 3000, oracle: true, mv_bps: if i < 150 { -440 } else { 440 } });
        }
    }
    let mut sim = Sim::new();
    sim.run(&ops);
    replay_matches(&sim);
}
