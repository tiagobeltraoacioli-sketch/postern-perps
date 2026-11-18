//! Round 2 (agent 430): regression pins against the state of `main` f0e3967.
//!
//! 1. The demo log committed in round 1 (`engine/vectors/demo`, 60 blocks: deposits, orders, fills, cancels,
//!    withdrawals, oracle rounds) must still replay to the SAME state root: every round-2 change (formulas moved to
//!    `calc`, the dust sweep before the bad-debt settlement, R395-39, R395-22, R430-1, the optional genesis field)
//!    leaves the behaviour on an ordinary log byte-identical, and the genesis encoding of a pre-0.2 genesis is
//!    unchanged.
//! 2. The engine's copy of the oracle rules runs against 394's vectors AT THEIR SOURCE (`node/oracle/vectors/`),
//!    not only against the copy taken in round 1 (`tests/fixtures/oracle-ccdb290`, kept): a change of either side
//!    fails here. The halt vectors are also driven through the whole engine (signed rounds, `apply_block`).
//! 3. The engine reads the compliance parameters AT THEIR SOURCE (`compliance/markets.json`, `risk.json`).
mod common;
use common::*;
use postern_perps_engine::codec::{decode_inputs, decode_payload, encode_input};
use postern_perps_engine::eip712::{digest, domain_separator, oracle_struct_hash, recover, sign_digest};
use postern_perps_engine::genesis::{from_compliance, Genesis};
use postern_perps_engine::hash::{hex0x, parse_addr};
use postern_perps_engine::oracle::{HaltReason, Status, ROUND_MS};
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;
use serde_json::Value;

#[test]
fn demo_log_of_round_1_replays_to_its_committed_root() {
    let g = Genesis::from_json(include_str!("../vectors/demo/genesis.json")).unwrap();
    assert_eq!(g.g.encoding(), include_str!("../vectors/demo/genesis.json").trim().as_bytes(), "the genesis encoding of a pre-0.2 genesis is unchanged");
    let expected = include_str!("../vectors/demo/expected-root.txt");
    let (at, root) = expected.trim().split_once(' ').unwrap();
    let at: u64 = at.parse().unwrap();
    let mut e = Engine::new(g);
    let mut last = String::new();
    let mut n = 0;
    for line in include_str!("../vectors/demo/log.jsonl").lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).unwrap();
        let h: u64 = v["height"].as_str().unwrap().parse().unwrap();
        let t: u64 = v["time"].as_str().unwrap().parse().unwrap();
        let inputs = decode_inputs(&hex::decode(&v["inputs"].as_str().unwrap()[2..]).unwrap()).unwrap();
        let out = e.apply_block(&Block { height: h, time: t, inputs }).unwrap();
        postern_perps_engine::invariants::check(&e).unwrap();
        // every state of the old log survives a snapshot round trip
        let back = Engine::restore(e.g.clone(), &e.snapshot(), Some(&out.state_root)).unwrap();
        assert_eq!(back.snapshot(), e.snapshot());
        if h == at {
            last = hex0x(&out.state_root);
        }
        n += 1;
    }
    assert_eq!(n, 60);
    assert_eq!(last, root, "state root of the round-1 demo log at height {at}");
}

fn live_oracle(name: &str) -> Value {
    let p = format!("{}/../node/oracle/vectors/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))).unwrap()
}

/// 394's halt.json through the WHOLE engine: a genesis with 394's parameters (n = 5, q = 3, defaults of README 8),
/// one block per vector step at its block time, the step's committee values signed by throwaway oracle keys (one
/// observation per value). After each block the engine's market status, index and exit band must be 394's.
#[test]
fn halt_vectors_at_their_source_through_apply_block() {
    let v = live_oracle("halt.json");
    let mut m = market(1, "BTC-USD");
    m.oracle = postern_perps_engine::genesis::OracleParams { min_sources: 3, agree_bps: 100, jump_bps: 500, max_staleness_ms: 10_000, confirm_rounds: 3, exit_haircut_bps: 500, exit_haircut_per_min_bps: 100, exit_max_haircut_bps: 5000, max_halt_ms: 3_600_000 };
    let mut g = genesis_raw(5, vec![m]);
    g.oracle_quorum = 3;
    g.oracle_max_age_ms = 6000;
    let mut e = Engine::new(g.check().unwrap());
    let keys = oracle_keys(5);
    let steps = v["steps"].as_array().unwrap();
    let (mut checked, mut halts, mut settle_due) = (0, 0, 0);
    for (i, st) in steps.iter().enumerate() {
        let t = st["blockTime"].as_u64().unwrap();
        let round = st["round"].as_u64().unwrap();
        let vals: Vec<u64> = st["committee"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect();
        let mut inputs = vec![];
        if vals.len() >= 3 {
            let time_ms = round * ROUND_MS;
            let obs = vals
                .iter()
                .enumerate()
                .map(|(s, p)| {
                    let prices = vec![OraclePrice { market_id: 1, price: *p, sources: 5 }];
                    let d = digest(&e.domain_sep, &oracle_struct_hash(round, time_ms, &prices));
                    SignedObservation { signer: s as u8, time_ms, prices, signature: sign_digest(&keys[s], &d) }
                })
                .collect();
            inputs.push(Input::Oracle(OracleUpdate { round, observations: obs }));
        }
        let was_active = e.markets[&1].oracle.status == Status::Active;
        let out = e.apply_block(&Block { height: i as u64 + 1, time: t, inputs }).unwrap();
        if let Some(r) = out.outcomes.first() {
            assert_eq!(*r, Ok(()), "step {i} (round {round}): the engine accepts the update ({})", st["outcome"]);
        }
        let halted_now = out.events.iter().any(|ev| matches!(ev, Event::Halt { market: 1 }));
        if st["haltedStaleAtBlockStart"].as_bool().unwrap() {
            assert!(halted_now && was_active, "step {i}: H1 halt at the block start");
        }
        halts += halted_now as usize;
        let o = &e.markets[&1].oracle;
        match (&st["exit"], o.exit_price(&e.markets[&1].p.oracle, t)) {
            (Value::Null, None) => {}
            (x, Some(got)) => {
                let want = (x["low"].as_u64().unwrap(), x["high"].as_u64().unwrap(), x["settle"].as_u64().unwrap(), x["settleDue"].as_bool().unwrap());
                assert_eq!((got.low, got.high, got.settle, got.settle_due), want, "step {i} (round {round}, {}): exit band", st["outcome"]);
                settle_due += got.settle_due as usize;
                checked += 1;
            }
            (x, None) => panic!("step {i}: the engine has no exit price, 394 says {x}"),
        }
        // 394's outcome names: Resumed / Moved leave the market active at that price; Confirming / HaltedJump leave
        // it halted; Disagree and withheld change nothing by themselves
        let oc = st["outcome"].as_str().unwrap();
        let price_of = |s: &str| -> u64 { s.split("price: ").nth(1).unwrap().split([',', ' ', '}']).next().unwrap().parse().unwrap() };
        if oc.starts_with("Resumed") || oc.starts_with("Moved") {
            assert_eq!((o.status, o.index), (Status::Active, price_of(oc)), "step {i}: {oc}");
        } else if oc.starts_with("Confirming") || oc.starts_with("HaltedJump") {
            assert_ne!(o.status, Status::Active, "step {i}: {oc}");
            assert_eq!(o.run.map(|r| r.0), Some(price_of(oc)), "step {i}: {oc}");
        } else {
            assert!(oc.starts_with("Disagree") || oc == "withheld", "step {i}: unknown outcome {oc} (re-read 394's vectors)");
        }
        assert_ne!(o.status, Status::Halted(HaltReason::Arithmetic));
        postern_perps_engine::invariants::check(&e).unwrap();
    }
    assert_eq!(steps.len(), 17, "394's halt.json has 17 steps (a change of the file must be re-read, not absorbed)");
    assert_eq!(checked, 15);
    assert!(halts >= 1, "the vectors halt the market at least once");
    let _ = settle_due;
}

#[test]
fn eip712_vectors_at_their_source() {
    let v = live_oracle("eip712.json");
    let vault = parse_addr(v["verifyingContract"].as_str().unwrap()).unwrap();
    let ds = domain_separator(700770, &vault);
    assert_eq!(hex0x(&ds), v["domainSeparator"].as_str().unwrap());
    let obs = v["observations"].as_array().unwrap();
    assert_eq!(obs.len(), 3);
    let mut signers = vec![];
    for o in obs {
        let m = &o["typedData"]["message"];
        let prices: Vec<OraclePrice> = m["prices"].as_array().unwrap().iter().map(|x| OraclePrice { market_id: x["marketId"].as_u64().unwrap() as u32, price: x["price"].as_u64().unwrap(), sources: x["sources"].as_u64().unwrap() as u8 }).collect();
        let sh = oracle_struct_hash(m["round"].as_u64().unwrap(), m["timeMs"].as_u64().unwrap(), &prices);
        assert_eq!(hex0x(&sh), o["hashStruct"].as_str().unwrap(), "{}", o["name"]);
        let d = digest(&ds, &sh);
        assert_eq!(hex0x(&d), o["digest"].as_str().unwrap(), "{}", o["name"]);
        let sig: [u8; 65] = hex::decode(&o["signature"].as_str().unwrap()[2..]).unwrap().try_into().unwrap();
        let who = recover(&d, &sig).expect("recovers");
        assert_eq!(hex0x(&who), o["signer"].as_str().unwrap().to_lowercase(), "{}", o["name"]);
        signers.push(who);
    }
    let bytes = hex::decode(&v["update"]["canonicalBytes"].as_str().unwrap()[2..]).unwrap();
    let input = decode_payload(0x01, &bytes).expect("394's canonical OracleUpdate decodes strictly");
    assert_eq!(&encode_input(&input)[5..], &bytes[..], "and re-encodes byte-identically");
    // Each of 394's signed observations as a one-signer round of an engine whose genesis names that signer and
    // 394's vault: accepted by the engine's committee rules (C1 to C2c: form, signature, round window, block-time
    // window); refused with ORACLE_BAD_SIGNATURE when one bit of the signature changes or under another vault.
    for (o, who) in obs.iter().zip(&signers) {
        let m = &o["typedData"]["message"];
        let prices: Vec<OraclePrice> = m["prices"].as_array().unwrap().iter().map(|x| OraclePrice { market_id: x["marketId"].as_u64().unwrap() as u32, price: x["price"].as_u64().unwrap(), sources: x["sources"].as_u64().unwrap() as u8 }).collect();
        let (round, time_ms) = (m["round"].as_u64().unwrap(), m["timeMs"].as_u64().unwrap());
        let sig: [u8; 65] = hex::decode(&o["signature"].as_str().unwrap()[2..]).unwrap().try_into().unwrap();
        let mk = |vault: &str| {
            let mut g = genesis_raw(1, vec![market(1, "BTC-USD"), market(2, "ETH-USD"), market(3, "SOL-USD")]);
            g.verifying_contract = vault.to_string();
            g.oracle_signers = vec![hex0x(who)];
            g.oracle_quorum = 1;
            Engine::new(g.check().unwrap())
        };
        let update = |sig: [u8; 65]| Input::Oracle(OracleUpdate { round, observations: vec![SignedObservation { signer: 0, time_ms, prices: prices.clone(), signature: sig }] });
        let t = time_ms.saturating_add(500);
        let mut e = mk(v["verifyingContract"].as_str().unwrap());
        let out = e.apply_block(&Block { height: 1, time: t, inputs: vec![update(sig)] }).unwrap();
        assert_eq!(out.outcomes, [Ok(())], "{}: accepted", o["name"]);
        for p in &prices {
            if let Some(mkt) = e.markets.get(&p.market_id) {
                // confirmRounds = 1 in the test market: the first committee price becomes the index
                assert_eq!((mkt.oracle.index, mkt.oracle.status), (p.price, Status::Active), "{}: market {}", o["name"], p.market_id);
            }
        }
        let mut bad = sig;
        bad[10] ^= 1;
        let mut e = mk(v["verifyingContract"].as_str().unwrap());
        let out = e.apply_block(&Block { height: 1, time: t, inputs: vec![update(bad)] }).unwrap();
        assert_eq!(out.outcomes, [Err(Error::OracleBadSignature)], "{}: altered signature", o["name"]);
        let mut e = mk(DEV_VAULT);
        let out = e.apply_block(&Block { height: 1, time: t, inputs: vec![update(sig)] }).unwrap();
        assert_eq!(out.outcomes, [Err(Error::OracleBadSignature)], "{}: another deployment's domain", o["name"]);
    }
}

fn live_compliance(name: &str) -> Value {
    let p = format!("{}/../compliance/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))).unwrap()
}

/// Task 2 of round 2: `fundingClamp`, the volume fee tiers and the launch gate reach the engine through
/// `genesis::from_compliance` from the files on `main`, value for value (no number is typed here: each is
/// converted from the file by an independent exact-decimal routine and compared).
#[test]
fn compliance_parameters_at_their_source() {
    let (mk, rk) = (live_compliance("markets.json"), live_compliance("risk.json"));
    let signers: Vec<String> = oracle_keys(7).iter().map(|k| hex0x(&addr(k))).collect();
    let g = from_compliance(&mk, &rk, DEV_VAULT, signers, 5, "25000", 250_000).expect("the compliance files on main convert");
    let units = |s: &str, d: u32| -> u64 {
        let (i, f) = s.split_once('.').unwrap_or((s, ""));
        assert!(f.len() as u32 <= d, "{s}");
        format!("{i}{f}{}", "0".repeat((d as usize) - f.len())).parse().unwrap()
    };
    let ms = mk["markets"].as_array().unwrap();
    assert_eq!(g.markets.len(), ms.len());
    for (em, fm) in g.markets.iter().zip(ms) {
        assert_eq!(em.funding_clamp, fm["fundingClamp"].as_i64().unwrap(), "{}: fundingClamp", em.symbol);
        assert_eq!(em.interest, fm["interest"].as_i64().unwrap());
        assert_eq!(em.max_funding_rate, fm["maxFundingRate"].as_i64().unwrap());
        assert!(em.funding_clamp <= em.max_funding_rate);
        assert_eq!(em.tick, units(fm["tick"].as_str().unwrap(), 8));
        assert_eq!(em.oi_cap, units(fm["oiCap"].as_str().unwrap(), 8));
        assert_eq!(em.tiers.len(), fm["tiers"].as_array().unwrap().len());
    }
    let ft = rk["feeTiers"]["tiers"].as_array().unwrap();
    assert_eq!(g.fee_tiers.len(), ft.len());
    for (et, f) in g.fee_tiers.iter().zip(ft) {
        assert_eq!(et.min_volume_14d, units(f["minVolume14d"].as_str().unwrap(), 6));
        assert_eq!((et.taker_fee_ppm as u64, et.maker_fee_ppm as u64), (f["takerFeePpm"].as_u64().unwrap(), f["makerFeePpm"].as_u64().unwrap()));
    }
    assert_eq!(g.launch_insurance_min, units(rk["insuranceFund"]["seedRequired"].as_str().unwrap(), 6), "launch gate: insurance seed");
    assert_eq!(g.launch_backstop_min, 25_000_000_000, "launch gate: the backstop seed is an explicit owner input");
    // the launch gate is live in an engine built from the files: an order is NOT_LAUNCHED until both seeds arrive
    let mut r = Rig::new(g.clone());
    let ka = key(1);
    let a = addr(&ka);
    let tick0 = g.markets[0].tick;
    let px = P / tick0 * tick0;
    let prices: Vec<(u32, u64)> = g.markets.iter().map(|m| (m.market_id, if m.market_id == 1 { px } else { m.tick * 1000 })).collect();
    let d = r.dep(a, 10_000);
    // confirmRounds of the files (3): three rounds before the markets are active
    r.block(3000, &prices, vec![d]);
    r.block(3000, &prices, vec![]);
    r.block(3000, &prices, vec![]);
    assert_eq!(r.e.markets[&1].oracle.status, Status::Active);
    let order = |r: &Rig, n: u64| sign_action(&r.e, &ka, place(a, r.t + n, 1, true, ORDER_LIMIT, TIF_GTC, px - 100 * tick0, g.markets[0].min_size, false));
    let o = order(&r, 1);
    let out = r.block(3000, &prices, vec![o]);
    assert_eq!(out.outcomes[1], Err(Error::NotLaunched));
    let seeds = vec![r.dep(postern_perps_engine::engine::system_address(1), g.launch_insurance_min / 1_000_000), r.dep(postern_perps_engine::engine::system_address(2), g.launch_backstop_min / 1_000_000)];
    let o = order(&r, 2);
    let mut inputs = seeds;
    inputs.push(o);
    let out = r.block(3000, &prices, inputs);
    assert_eq!(out.outcomes[3], Ok(()), "{:?}", out.outcomes);
    assert!(r.e.launched);
    // the volume tiers are applied by the engine: tier of a volume just below / at each floor
    for (i, t) in g.fee_tiers.iter().enumerate() {
        let ai = r.ai(&a);
        let day = r.e.time / 86_400_000;
        r.e.accounts[ai as usize].volume.clear();
        r.e.accounts[ai as usize].volume.insert(day - 1, t.min_volume_14d as u128);
        assert_eq!((r.e.fee_ppm(ai, true), r.e.fee_ppm(ai, false)), (t.taker_fee_ppm, t.maker_fee_ppm), "tier {i} at its floor");
        if i > 0 {
            r.e.accounts[ai as usize].volume.insert(day - 1, t.min_volume_14d as u128 - 1);
            assert_eq!(r.e.fee_ppm(ai, true), g.fee_tiers[i - 1].taker_fee_ppm, "one micro-USDC below the floor of tier {i}");
        }
    }
}
