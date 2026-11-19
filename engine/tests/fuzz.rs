//! Property fuzzer: random traders, orders, cancels, withdrawals, deposits, leverage changes and oracle moves
//! (including halts and large drops that liquidate). After every block: the invariants of SPEC section 8. At the
//! end: the whole log re-encoded to canonical bytes, decoded and replayed on a fresh engine gives the same state
//! root at every height (I-5, codec canonicality).
//!
//! Round 2 (agent 430, SPEC 0.2) adds, after EVERY block of every run (nothing of the above removed):
//! - P-1 conservation from an independent ledger: deposits counted from the INPUTS the engine accepted and
//!   withdrawals from the records it emitted, never from the engine's own totals; I-1 and I-2 recomputed here;
//! - P-2 `sum(escapeValue) <= custody + deficit the insurance fund could not absorb`, and `<= custody` outright
//!   whenever the fund covers the deficits; `totalEscape` equals the sum of the leaves' values;
//! - P-3 no account ends a block below maintenance unless step 9 stopped at its pass bound (the maintenance
//!   requirement is recomputed here from the genesis tiers, not taken from the engine);
//! - P-4 snapshot: `state_root(restore(snapshot(s))) == state_root(s)`, byte-identical re-encoding, and (at the
//!   end of the run) an engine restored from a mid-run snapshot replays the rest of the log to the same roots and
//!   log hashes;
//! - P-5 replay through the codec also reproduces every log hash and every per-input outcome.
//! - P-6 (SPEC 6.5) the backstop's loss cap: after a block whose step 9 ran to completion the backstop holds no
//!   position in an active market unless its index-valued equity is at or above its stop level (floor from the
//!   genesis numbers, its own maintenance requirement and the buffer, all recomputed here).
//! - P-7 (SPEC 6.5, 0.2.6) the floor HOLDS on the active path: with every market active at the end of the previous
//!   block the backstop's equity does not fall below min(its previous equity, the floor).
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
        1 => (0..TRADERS, 1u64..2_000_000_000).prop_map(|(who, amount)| Op::Deposit { who, amount }),
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
    stats: std::collections::BTreeMap<String, u64>,
    /// P-1: independent ledger (USDC units).
    ledger_deposits: i128,
    ledger_withdrawn: i128,
    log_hashes: Vec<[u8; 32]>,
    outcomes: Vec<Vec<Result<(), Error>>>,
    /// P-4: a snapshot taken in the middle of the run (height, bytes).
    mid_snapshot: Option<(u64, Vec<u8>)>,
    /// P-7: the backstop's index-valued equity at the end of the previous block and whether the active-path
    /// condition held there.
    backstop_prev: Option<(i128, bool)>,
}

/// P-3: maintenance requirement of one position from the genesis tiers (SPEC 6.2), written independently of
/// `engine.rs`: the part of the notional inside each tier times its fraction, summed, divided by 10^6 rounding up.
fn mmr_independent(notional: i128, tiers: &[postern_perps_engine::genesis::Tier]) -> i128 {
    let mut acc: i128 = 0;
    for (i, t) in tiers.iter().enumerate() {
        let lo = t.floor as i128 * 10_000_000_000;
        let hi = tiers.get(i + 1).map(|n| n.floor as i128 * 10_000_000_000);
        let top = match hi {
            Some(h) => notional.min(h),
            None => notional,
        };
        if top > lo {
            acc += (top - lo) * t.mmf_ppm as i128;
        }
    }
    (acc + 999_999) / 1_000_000
}

fn round2_properties(sim: &mut Sim, b: &Block, out: &BlockOutcome) {
    let e = &sim.e;
    let h = sim.h;
    // ---- P-1
    for (i, o) in b.inputs.iter().zip(&out.outcomes) {
        if let (Input::Deposit { amount, .. }, Ok(())) = (i, o) {
            sim.ledger_deposits += *amount as i128;
        }
    }
    for ev in &out.events {
        if let Event::Withdrawal(w) = ev {
            sim.ledger_withdrawn += w.amount as i128;
        }
    }
    let custody = sim.ledger_deposits - sim.ledger_withdrawn;
    assert!(custody >= 0, "P-1: more withdrawn than deposited at height {h}");
    let mut balances: i128 = 0;
    let mut open_notional: i128 = 0;
    let mut sizes: std::collections::BTreeMap<u32, i128> = Default::default();
    for a in &e.accounts {
        balances += a.balance;
        for (mid, p) in &a.positions {
            open_notional += p.open_notional;
            *sizes.entry(*mid).or_default() += p.size as i128;
        }
    }
    for (mid, s) in &sizes {
        assert_eq!(*s, 0, "P-1 / I-1: sizes of market {mid} do not sum to zero at height {h}");
    }
    assert_eq!(10_000_000_000 * balances + e.dust - open_notional, 10_000_000_000 * custody, "P-1 / I-2 against the independent ledger at height {h}");
    assert!(e.dust >= 0 && e.dust < 10_000_000_000, "dust is swept every block: {}", e.dust);
    let recorded: i128 = e.withdrawals.iter().map(|w| w.amount as i128).sum();
    assert_eq!(recorded, sim.ledger_withdrawn, "P-1: withdrawal tree vs emitted records at height {h}");
    // ---- P-2
    let esc = e.escape_values();
    let total: i128 = esc.iter().map(|x| *x as i128).sum();
    let mut deficit: i128 = 0;
    for ai in 0..e.accounts.len() as u32 {
        let eq = e.equity(ai);
        if eq < 0 {
            deficit += (-eq + 9_999_999_999) / 10_000_000_000;
        }
    }
    let ins = (e.equity(1).max(0)) / 10_000_000_000;
    let uncovered = (deficit - ins).max(0);
    assert!(total <= custody + uncovered, "P-2: escape total {total} > custody {custody} + uncovered {uncovered} at height {h}");
    if uncovered == 0 {
        assert!(total <= custody, "P-2: escape total {total} > custody {custody} with the deficit covered at height {h}");
    } else {
        *sim.stats.entry("p2:uncovered-deficit".into()).or_default() += 1;
    }
    if deficit > 0 {
        *sim.stats.entry("p2:deficit".into()).or_default() += 1;
    }
    // ---- P-3
    let (passes, complete) = e.last_passes;
    assert!(passes >= 1 && passes <= postern_perps_engine::engine::MAX_SYSTEM_PASSES);
    for ai in 3..e.accounts.len() as u32 {
        let a = &e.accounts[ai as usize];
        if a.positions.is_empty() {
            continue;
        }
        let all_active = a.positions.keys().all(|m| e.markets[m].oracle.status == postern_perps_engine::oracle::Status::Active && e.markets[m].mark > 0);
        if !all_active {
            *sim.stats.entry("p3:skipped-halted".into()).or_default() += 1;
            continue; // SPEC 6.3: not liquidated while one of its markets is halted (stated)
        }
        let mr: i128 = a.positions.iter().map(|(m, p)| mmr_independent((p.size as i128).abs() * e.markets[m].mark as i128, &e.markets[m].p.tiers)).sum();
        if e.equity(ai) < mr {
            assert!(!complete, "P-3: account {ai} below maintenance at the end of height {h} although step 9 ran to completion");
            *sim.stats.entry("p3:carried-to-next-block".into()).or_default() += 1;
        }
    }
    if !complete {
        *sim.stats.entry("p3:pass-bound-hit".into()).or_default() += 1;
    }
    // ---- P-6 and P-7 (SPEC 6.5), recomputed here from the genesis numbers and the raw state
    {
        let bs = &e.accounts[2];
        let active = |m: &u32| e.markets[m].oracle.status == postern_perps_engine::oracle::Status::Active && e.markets[m].mark > 0;
        let budget = e.g.g.backstop_loss_budget_ppm.unwrap_or(1_000_000) as i128;
        let floor = (e.g.g.launch_backstop_min as i128 * (1_000_000 - budget) + 999_999) / 1_000_000 * 10_000_000_000;
        let mr: i128 = bs.positions.iter().map(|(m, p)| mmr_independent((p.size as i128).abs() * e.markets[m].mark as i128, &e.markets[m].p.tiers)).sum();
        // index-valued equity and buffer (jump + mark band + half the tier-0 maintenance + one capped funding)
        let mut eq = bs.balance * 10_000_000_000;
        let mut buffer: i128 = 0;
        for (m, p) in &bs.positions {
            let mk = &e.markets[m];
            let price = if active(m) { mk.oracle.index } else { mk.mark } as i128;
            eq += p.size as i128 * price - p.open_notional;
            if active(m) {
                let ppm = mk.p.oracle.jump_bps as i128 * 100 + mk.p.mark_band_bps as i128 * 100 + (mk.p.tiers[0].mmf_ppm as i128 + 1) / 2 + (mk.p.max_funding_rate as i128 + 999) / 1000;
                buffer += ((p.size as i128).abs() * mk.oracle.index as i128 * ppm + 999_999) / 1_000_000;
            }
        }
        let level = mr.max(floor) + buffer;
        let holds_active = bs.positions.keys().any(active);
        if holds_active && eq < level {
            assert!(!complete, "P-6: BACKSTOP holds a position in an active market with equity {eq} below its stop level {level} at the end of height {h}");
            *sim.stats.entry("p6:carried-to-next-block".into()).or_default() += 1;
        }
        let holds_halted = bs.positions.keys().any(|m| !active(m));
        if holds_halted {
            *sim.stats.entry("p6:position-in-a-halted-market".into()).or_default() += 1;
        }
        // P-7: on the active path the floor HOLDS. If every market was active at the end of the previous block and
        // the backstop held and holds nothing in a halted market, and both blocks completed step 9, its equity is
        // not below min(its equity one block earlier, the floor), up to rounding (100 USDC units).
        let all_active_now = e.markets.keys().all(|m| active(m));
        if let Some((prev_eq, prev_ok)) = sim.backstop_prev {
            if prev_ok && complete && !holds_halted {
                assert!(eq >= prev_eq.min(floor) - 100 * 10_000_000_000, "P-7: BACKSTOP equity {eq} fell below min(previous {prev_eq}, floor {floor}) on the active path at height {h}");
                *sim.stats.entry("p7:checked".into()).or_default() += 1;
            }
        }
        sim.backstop_prev = Some((eq, all_active_now && complete && !holds_halted));
        if eq < floor {
            *sim.stats.entry("backstop:below-floor".into()).or_default() += 1;
        }
    }
    // R430-1 / R436-5: the backstop's state is visible in the statistics
    let bs = &e.accounts[2];
    if !bs.positions.is_empty() {
        *sim.stats.entry("backstop:holds-position".into()).or_default() += 1;
        if e.equity(2) < 0 {
            *sim.stats.entry("backstop:under-water".into()).or_default() += 1;
        }
    }
    if bs.balance < 0 {
        *sim.stats.entry("backstop:negative-balance".into()).or_default() += 1;
    }
    for ev in &out.events {
        if matches!(ev, Event::BadDebt { account: 2, .. }) {
            *sim.stats.entry("backstop:bad-debt-event".into()).or_default() += 1;
        }
    }
    // ---- P-4
    let snap = e.snapshot();
    let back = Engine::restore(e.g.clone(), &snap, Some(&out.state_root)).unwrap_or_else(|er| panic!("P-4: restore failed at height {h}: {er}"));
    assert_eq!(back.state_root(), out.state_root, "P-4 at height {h}");
    assert_eq!(back.snapshot(), snap, "P-4: re-encoding at height {h}");
    assert_eq!((back.log_hash, back.total_deposits, back.height, back.time), (e.log_hash, e.total_deposits, e.height, e.time));
    if h % 7 == 3 || sim.mid_snapshot.is_none() {
        sim.mid_snapshot = Some((h, snap));
    }
    sim.log_hashes.push(out.log_hash);
    sim.outcomes.push(out.outcomes.clone());
}

fn round_tick(p: u64, tick: u64) -> u64 {
    (p / tick).max(1) * tick
}

impl Sim {
    fn new() -> Sim {
        Sim::seeded(0, 0)
    }
    /// As `new`, with INSURANCE and BACKSTOP funded through deposits in the first block (review 436's request with
    /// R436-5: the generator must reach a backstop that holds positions, loses on them and goes negative).
    fn seeded(insurance_usdc: u64, backstop_usdc: u64) -> Sim {
        let mut sim = Sim::unseeded();
        for (i, usdc) in [(1u32, insurance_usdc), (2, backstop_usdc)] {
            if usdc > 0 {
                sim.deposits.push(Input::Deposit { index: sim.dep, account: postern_perps_engine::engine::system_address(i), amount: usdc * 1_000_000 });
                sim.dep += 1;
            }
        }
        sim
    }
    fn unseeded() -> Sim {
        let mut m2 = market(2, "ETH-USD");
        m2.tick = 1_000_000;
        m2.step = 1_000_000;
        m2.min_size = 1_000_000;
        m2.max_size = 100_000_000_000;
        m2.oi_cap = 2_000_000_000_000;
        let e = Engine::new(genesis(5, vec![market(1, "BTC-USD"), m2]));
        Sim { e, ok: oracle_keys(5), keys: (0..TRADERS as u32).map(|i| key(100 + i)).collect(), t: T0, h: 0, dep: 0, px: [0, 6_725_000_000_000, 348_000_000_000], pending: vec![], deposits: vec![], nonce: 0, log: vec![], roots: vec![], stats: Default::default(), ledger_deposits: 0, ledger_withdrawn: 0, log_hashes: vec![], outcomes: vec![], mid_snapshot: None, backstop_prev: None }
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
        for ev in &out.events {
            let name = match ev {
                Event::Fill { kind, .. } => format!("fill:{kind:?}"),
                other => format!("{other:?}").split([' ', '{', '(']).next().unwrap().to_string(),
            };
            *self.stats.entry(name).or_default() += 1;
        }
        for o in &out.outcomes {
            let k = match o { Ok(()) => "ok".to_string(), Err(e) => format!("err:{}", e.name()) };
            *self.stats.entry(k).or_default() += 1;
        }
        self.roots.push(out.state_root);
        round2_properties(self, &b, &out);
        self.log.push(b);
    }
    fn run(&mut self, ops: &[Op]) {
        for who in 0..TRADERS {
            let a = addr(&self.keys[who]);
            self.deposits.push(Input::Deposit { index: self.dep, account: a, amount: if who < 2 { 50_000_000_000 } else { 3_000_000_000 } });
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
                    if dt > 60_000 {
                        // signed actions carry a nonce near the current time: include them first, then let the
                        // long gap pass (an hour: funding boundary) in an empty block
                        self.flush(1000, oracle);
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
    for (i, (b, root)) in sim.log.iter().zip(&sim.roots).enumerate() {
        let bytes = encode_inputs(&b.inputs);
        let decoded = decode_inputs(&bytes).expect("canonical bytes decode");
        assert_eq!(encode_inputs(&decoded), bytes, "P-5: decode then encode is the identity");
        let out = fresh.apply_block(&Block { height: b.height, time: b.time, inputs: decoded }).unwrap();
        assert_eq!(&out.state_root, root, "replay diverged at height {}", b.height);
        assert_eq!(out.log_hash, sim.log_hashes[i], "P-5: log hash at height {}", b.height);
        assert_eq!(out.outcomes, sim.outcomes[i], "P-5: outcomes at height {}", b.height);
    }
    assert_eq!(fresh.snapshot(), sim.e.snapshot(), "P-5: the replayed engine is byte-equal to the original in every field a snapshot holds");
    // P-4: recovery from a mid-run snapshot, then the rest of the log, reaches the same roots and log hashes
    let (h0, snap) = sim.mid_snapshot.clone().expect("a snapshot was taken");
    let mut resumed = Engine::restore(sim.e.g.clone(), &snap, Some(&sim.roots[h0 as usize - 1])).expect("mid-run snapshot restores");
    for b in sim.log.iter().filter(|b| b.height > h0) {
        let decoded = decode_inputs(&encode_inputs(&b.inputs)).unwrap();
        let out = resumed.apply_block(&Block { height: b.height, time: b.time, inputs: decoded }).unwrap();
        assert_eq!(out.state_root, sim.roots[b.height as usize - 1], "P-4: resumed engine diverged at height {}", b.height);
        assert_eq!(out.log_hash, sim.log_hashes[b.height as usize - 1], "P-4: resumed log hash at height {}", b.height);
        invariants::check(&resumed).expect("invariants after resuming from a snapshot");
    }
    assert_eq!(resumed.snapshot(), sim.e.snapshot(), "P-4: resumed and original engines are byte-equal");
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
        if i % 100 == 50 {
            ops.push(Op::Block { dt: 3_600_000, oracle: true, mv_bps: 0 });
        }
    }
    let mut sim = Sim::new();
    sim.run(&ops);
    replay_matches(&sim);
    eprintln!("crash_and_recover stats: {:?}", sim.stats);
    for k in ["fill:Book", "Liquidated", "Funding", "Rested", "ok"] {
        assert!(sim.stats.get(k).copied().unwrap_or(0) > 0, "path {k} not exercised: {:?}", sim.stats);
    }
    assert!(sim.stats.get("fill:Adl").copied().unwrap_or(0) + sim.stats.get("fill:Liquidation").copied().unwrap_or(0) > 0, "no liquidation fill: {:?}", sim.stats);
}

/// Aggregate path coverage over many random runs (so a fuzzer that never reaches a path is visible).
#[test]
fn random_runs_reach_every_path() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strat = prop::collection::vec(op(), 150..250);
    let mut total: std::collections::BTreeMap<String, u64> = Default::default();
    for _ in 0..24 {
        let ops = strat.new_tree(&mut runner).unwrap().current();
        let mut sim = Sim::new();
        sim.run(&ops);
        for (k, v) in sim.stats {
            *total.entry(k).or_default() += v;
        }
    }
    eprintln!("random coverage: {total:?}");
    for k in ["fill:Book", "Rested", "Triggered", "TriggerStored", "Cancelled", "Withdrawal", "Halt", "Resume", "Funding", "Liquidated", "err:INSUFFICIENT_MARGIN", "err:REDUCE_ONLY", "err:WOULD_CROSS", "err:MARKET_HALTED"] {
        assert!(total.get(k).copied().unwrap_or(0) > 0, "random fuzzer never reached {k}: {total:?}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: std::env::var("FUZZ_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(32), max_shrink_iters: 2000, .. ProptestConfig::default() })]
    /// The same properties with a funded insurance fund and a SMALL backstop, so that liquidation remainders go to
    /// the backstop and its losses can exceed its balance (R430-1 / R436-5).
    #[test]
    fn invariants_and_replay_with_a_small_backstop(ops in prop::collection::vec(op(), 1..250)) {
        let mut sim = Sim::seeded(300, 400);
        sim.run(&ops);
        replay_matches(&sim);
    }
}

/// Review 436's request with R436-5 (the state R430-1 fixed): the suite must REACH a backstop that takes positions,
/// goes under water and ends with a negative balance that is reported and settled. Deterministic: thin books (IOC
/// only after the positions are open), a fall that liquidates the longs into the backstop, a further fall that
/// puts the backstop under water, then a rise that liquidates the shorts against it (ADL: the backstop first).
/// Every block runs every property of this file, `invariants::check` included.
#[test]
fn backstop_takes_positions_goes_under_water_and_its_debt_is_reported() {
    let mut ops = vec![];
    // open: traders 2..5 (3,000 USDC each) long against traders 0, 1 (50,000 each), then cancel everything resting
    for i in 0..40u64 {
        let long = 2 + (i % 4) as usize;
        let short = (i % 2) as usize;
        ops.push(Op::Place { who: short, mkt: 1, buy: false, ty: 0, tif: 0, off: 0, steps: 60, ro: false, trig_off: 0 });
        ops.push(Op::Place { who: long, mkt: 1, buy: true, ty: 1, tif: 1, off: 30, steps: 60, ro: false, trig_off: 0 });
        if i % 8 == 7 {
            ops.push(Op::Block { dt: 3000, oracle: true, mv_bps: 0 });
        }
    }
    for who in 0..TRADERS {
        ops.push(Op::CancelAll { who });
    }
    ops.push(Op::Block { dt: 3000, oracle: true, mv_bps: 0 });
    // fall, first in 0.5% steps: the longs (about ten times leveraged on 3,000 USDC each) cross their maintenance
    // requirement with equity still positive, the book is empty, so the backstop takes what its own margin allows
    // (the rest: ADL); then in 4% steps, which put the backstop far under water
    for i in 0..34 {
        ops.push(Op::Block { dt: 3000, oracle: true, mv_bps: if i < 24 { -50 } else { -400 } });
    }
    // rise: new shorts are opened low with little margin (800 USDC more on about 0.15 BTC) and the index rises in
    // 4.2% steps until they are liquidated: the book is empty and the backstop cannot take (under water), so they
    // are deleveraged, and the backstop, long from the fall, is the first counterparty (R395-14): it realises its
    // loss, far beyond its 400 USDC
    for i in 0..24u64 {
        let short = 2 + (i % 4) as usize;
        ops.push(Op::Deposit { who: short, amount: 800_000_000 });
        ops.push(Op::Block { dt: 3000, oracle: true, mv_bps: 0 });
        ops.push(Op::Place { who: (i % 2) as usize, mkt: 1, buy: true, ty: 0, tif: 0, off: 0, steps: 150, ro: false, trig_off: 0 });
        ops.push(Op::Place { who: short, mkt: 1, buy: false, ty: 1, tif: 1, off: -30, steps: 150, ro: false, trig_off: 0 });
        ops.push(Op::Block { dt: 3000, oracle: true, mv_bps: 0 });
        ops.push(Op::CancelAll { who: (i % 2) as usize });
        ops.push(Op::Block { dt: 3000, oracle: true, mv_bps: 420 });
        ops.push(Op::Block { dt: 3000, oracle: true, mv_bps: 420 });
        ops.push(Op::Block { dt: 3000, oracle: true, mv_bps: 420 });
    }
    let mut sim = Sim::seeded(50, 400);
    sim.run(&ops);
    replay_matches(&sim);
    eprintln!("backstop sequence stats: {:?}", sim.stats);
    // Restated at SPEC 0.2.5 (loss cap, SPEC 6.5). Until 0.2.4 this sequence had to reach "backstop:under-water"
    // (a position held with negative equity, for 33 blocks): that state is now forbidden while the market is
    // active (P-6 in every block) and must NOT be reached; instead the backstop must be seen STOPPED. Kept: it
    // takes positions and loses on them. NOT kept as a requirement of THIS sequence: "backstop:negative-balance"
    // and "backstop:bad-debt-event". With the cap the backstop is stopped (twice here) while its equity is still
    // positive, so this sequence no longer produces a debt at all; the debt path (a single move larger than its
    // equity, reported as BadDebt and paid by INSURANCE) stays covered by
    // `r430_1_backstop_loss_beyond_its_balance_is_bad_debt_paid_by_insurance` (tests/r2_failing_first.rs).
    for k in ["fill:Backstop", "backstop:holds-position", "BackstopStopped", "Liquidated"] {
        assert!(sim.stats.get(k).copied().unwrap_or(0) > 0, "the sequence never reached {k}: {:?}", sim.stats);
    }
    assert_eq!(sim.stats.get("backstop:under-water").copied().unwrap_or(0), 0, "the backstop never ends a block under water with a position: {:?}", sim.stats);
}
