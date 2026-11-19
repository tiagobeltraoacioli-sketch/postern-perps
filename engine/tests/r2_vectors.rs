//! Round 2 (agent 430): the vectors other areas build against are (1) exactly what the engine produces now (the
//! committed files cannot drift), (2) tied to what the STATE MACHINE does, not only to the formula module, and
//! (3) verifiable without the engine (independent Merkle and state-root recomputation here; an independent Python
//! implementation in `engine/vectors/check-worked-examples.py`).
mod common;
use common::*;
use postern_perps_engine::calc;
use postern_perps_engine::engine::{Event, FillKind};
use postern_perps_engine::hash::{keccak, keccak_parts, H256};
use postern_perps_engine::num::N_PER_USDC;
use postern_perps_engine::testkit::*;
use postern_perps_engine::vectors;
use postern_perps_engine::*;
use serde_json::Value;

fn same_as_committed(name: &str, committed: &str, now: &Value) {
    let fresh = serde_json::to_string_pretty(now).unwrap();
    assert!(committed.trim_end() == fresh.trim_end(), "engine/vectors/{name} is not what the engine produces now: regenerate it with the `engine` CLI and tell the areas that read it (cross-area log)");
}

#[test]
fn worked_examples_file_is_current() {
    same_as_committed("worked-examples.json", include_str!("../vectors/worked-examples.json"), &vectors::worked_examples());
    let v: Value = serde_json::from_str(include_str!("../vectors/worked-examples.json")).unwrap();
    for k in ["fees", "margin", "liquidationPrice", "liquidationExecution", "fundingRate", "fundingPayment", "premium", "mark", "pnl"] {
        assert!(v[k].as_array().is_some_and(|a| a.len() >= 5), "{k}");
    }
    // SPEC 15's numbers are the first cases
    assert_eq!(v["fees"][0]["fee"], "15131250");
    assert_eq!((v["margin"][0]["initialMargin"].as_str(), v["margin"][0]["maintenanceMargin"].as_str()), (Some("3362500000"), Some("840625000")), "0.5 BTC at 67,250 with the compliance tiers: IMF max(5%, 10%) and MMF 2.5%");
}

#[test]
fn snapshot_example_file_is_current_and_loads() {
    let committed = include_str!("../vectors/snapshot-v1.json");
    same_as_committed("snapshot-v1.json", committed, &vectors::snapshot_example());
    let v: Value = serde_json::from_str(committed).unwrap();
    let g = postern_perps_engine::genesis::Genesis::from_json(v["genesis"].as_str().unwrap()).unwrap();
    let bytes = hex::decode(&v["snapshot"].as_str().unwrap()[2..]).unwrap();
    let root: H256 = hex::decode(&v["stateRoot"].as_str().unwrap()[2..]).unwrap().try_into().unwrap();
    let e = Engine::restore(g.clone(), &bytes, Some(&root)).expect("the committed snapshot loads to its root");
    assert_eq!(postern_perps_engine::hash::hex0x(&e.log_hash), v["logHash"].as_str().unwrap());
    // replaying the committed log from the committed genesis gives the same bytes
    let mut r = Engine::new(g);
    for b in v["log"].as_array().unwrap() {
        let inputs = postern_perps_engine::codec::decode_inputs(&hex::decode(&b["inputs"].as_str().unwrap()[2..]).unwrap()).unwrap();
        r.apply_block(&Block { height: b["height"].as_str().unwrap().parse().unwrap(), time: b["time"].as_str().unwrap().parse().unwrap(), inputs }).unwrap();
    }
    assert_eq!(r.snapshot(), bytes);
}

fn hx(v: &Value) -> Vec<u8> {
    hex::decode(&v.as_str().unwrap()[2..]).unwrap()
}

/// SPEC 11 proof verification written here from the text (not `merkle.rs`).
fn root_from_proof(leaf_data: &[u8], index: u64, siblings: &[Value]) -> H256 {
    let mut pre = vec![0u8];
    pre.extend_from_slice(leaf_data);
    let mut node = keccak(&pre);
    for (level, sib) in siblings.iter().enumerate() {
        let sbytes = hx(sib);
        node = if (index >> level) & 1 == 1 { keccak_parts(&[&[1u8], &sbytes, &node]) } else { keccak_parts(&[&[1u8], &node, &sbytes]) };
    }
    node
}

#[test]
fn proofs_example_file_is_current_and_verifies_independently() {
    let committed = include_str!("../vectors/proofs-example.json");
    let (_, _, e) = vectors::rehearsal();
    same_as_committed("proofs-example.json", committed, &vectors::proofs_json(&e));
    let v: Value = serde_json::from_str(committed).unwrap();
    let cp = &v["checkpoint"];
    let u = |k: &str| -> u128 { cp[k].as_str().unwrap().parse().unwrap() };
    // systemHash and stateRoot from the 16 checkpoint fields, per SPEC 11 / VAULT.md 5.2
    let sys = keccak_parts(&[
        b"PPv1sys",
        &(u("nextDepositIndex") as u64).to_be_bytes(),
        &(u("nextForcedIndex") as u64).to_be_bytes(),
        &(u("nextOrderId") as u64).to_be_bytes(),
        &u("totalEscape").to_be_bytes(),
        &u("dust").to_be_bytes(),
        &hx(&cp["auxHash"]),
        &(u("withdrawalCount") as u64).to_be_bytes(),
        &(u("withdrawalsTotal") as u64).to_be_bytes(),
    ]);
    assert_eq!(sys.to_vec(), hx(&v["systemHash"]));
    let root = keccak_parts(&[b"PosternPerps/state/v1", &(u("height") as u64).to_be_bytes(), &(u("time") as u64).to_be_bytes(), &hx(&cp["accountsRoot"]), &hx(&cp["marketsRoot"]), &hx(&cp["withdrawalsRoot"]), &sys]);
    assert_eq!(root.to_vec(), hx(&v["stateRoot"]));
    assert_eq!(root, e.state_root());
    // every account leaf: bytes rebuilt from its fields, proof to accountsRoot
    let accounts = v["accounts"].as_array().unwrap();
    let mut total: u128 = 0;
    let mut under_water = 0;
    for a in accounts {
        let idx = a["index"].as_u64().unwrap();
        let bal: i128 = a["balance"].as_str().unwrap().parse().unwrap();
        let esc: u64 = a["escapeValue"].as_str().unwrap().parse().unwrap();
        let mut data = b"PPv1acct".to_vec();
        data.extend_from_slice(&hx(&a["account"]));
        data.extend_from_slice(&(idx as u32).to_be_bytes());
        data.extend_from_slice(&bal.to_be_bytes());
        data.extend_from_slice(&esc.to_be_bytes());
        data.extend_from_slice(&hx(&a["positionsHash"]));
        assert_eq!(data, hx(&a["leafData"]), "account {idx}: leaf bytes");
        let sibs = a["siblings"].as_array().unwrap();
        assert_eq!(sibs.len(), 32);
        assert_eq!(root_from_proof(&data, idx, sibs).to_vec(), hx(&cp["accountsRoot"]), "account {idx}: proof");
        total += esc as u128;
        let eq: i128 = a["equityNotional"].as_str().unwrap().parse().unwrap();
        if eq < 0 {
            under_water += 1;
            assert_eq!(esc, 0);
        }
    }
    assert_eq!(total, u("totalEscape"), "totalEscape is the sum of the leaves' escape values");
    let custody: u128 = v["custody"].as_str().unwrap().parse().unwrap();
    assert!(total <= custody, "escape total {total} <= custody {custody}");
    // insurance first (R395-23): an account is under water and INSURANCE's escape value is its equity minus that
    assert!(under_water >= 1, "the example shows an underwater account");
    let ins = &accounts[1];
    let ins_eq: i128 = ins["equityNotional"].as_str().unwrap().parse().unwrap();
    let ins_esc: i128 = ins["escapeValue"].as_str().unwrap().parse().unwrap();
    let deficit: i128 = accounts.iter().map(|a| a["equityNotional"].as_str().unwrap().parse::<i128>().unwrap()).filter(|x| *x < 0).map(|x| (-x + N_PER_USDC - 1) / N_PER_USDC).sum();
    assert!(deficit > 0);
    assert_eq!(ins_esc, ins_eq / N_PER_USDC - deficit, "INSURANCE absorbs the whole deficit here");
    // every withdrawal record: bytes and proof to withdrawalsRoot
    let ws = v["withdrawals"].as_array().unwrap();
    assert!(ws.len() >= 2);
    let mut wsum: u128 = 0;
    for w in ws {
        let id: u64 = w["withdrawalId"].as_str().unwrap().parse().unwrap();
        let amount: u64 = w["amount"].as_str().unwrap().parse().unwrap();
        let mut data = b"PPv1wdrl".to_vec();
        data.extend_from_slice(&id.to_be_bytes());
        data.extend_from_slice(&hx(&w["account"]));
        data.extend_from_slice(&hx(&w["recipient"]));
        data.extend_from_slice(&amount.to_be_bytes());
        data.extend_from_slice(&w["height"].as_str().unwrap().parse::<u64>().unwrap().to_be_bytes());
        assert_eq!(data, hx(&w["leafData"]));
        assert_eq!(root_from_proof(&data, id, w["siblings"].as_array().unwrap()).to_vec(), hx(&cp["withdrawalsRoot"]), "withdrawal {id}: proof");
        wsum += amount as u128;
    }
    assert_eq!(wsum, u("withdrawalsTotal"));
    assert_eq!(ws.len() as u128, u("withdrawalCount"));
}

/// The formulas of `calc` are what the state machine does: fee of a fill, initial requirement behind free
/// collateral, funding payments, and the liquidation price as the exact threshold of a liquidation.
#[test]
fn state_machine_matches_the_formulas() {
    let mut r = Rig::one();
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let (d1, d2) = (r.dep(a, 3_500), r.dep(b, 100_000));
    // start 12 s before a funding boundary so that the trade and one sample precede it
    let to_boundary = 3_600_000 - (r.t % 3_600_000);
    r.block(to_boundary - 12_000, &[(1, P)], vec![d1, d2]);
    let out = r.trade(&[(1, P)], 1, &kb, &ka, P, 50_000_000);
    let n = calc::notional(P, 50_000_000);
    let (tf, mf) = out.events.iter().find_map(|e| match e { Event::Fill { kind: FillKind::Book, taker_fee, maker_fee, .. } => Some((*taker_fee, *maker_fee)), _ => None }).unwrap();
    assert_eq!((tf, mf), (calc::trade_fee(n, 450), calc::trade_fee(n, 150)));
    assert_eq!(tf, 15_131_250, "SPEC 15");
    let tiers = r.e.markets[&1].p.tiers.clone();
    let (ai, bi) = (r.ai(&a), r.ai(&b));
    assert_eq!(r.bal(&a), 3_500_000_000 - tf);
    assert_eq!(r.e.free_collateral(ai), r.bal(&a) - calc::req_usdc(calc::initial_req(n, &tiers, 10)), "free collateral = balance - initial margin at leverage 10 (no unrealised result at the entry price)");
    // funding at the boundary: empty book, premium 0, rate = clamp(interest) = 12,500e-9
    let (ba, bb) = (r.bal(&a), r.bal(&b));
    let mut rate = None;
    for _ in 0..5 {
        let out = r.block(3000, &[(1, P)], vec![]);
        if let Some(x) = out.events.iter().find_map(|e| match e { Event::Funding { rate, .. } => Some(*rate), _ => None }) {
            rate = Some(x);
            break;
        }
    }
    let rate = rate.expect("a funding event");
    assert_eq!(Some(rate), calc::funding_rate(&[0], 12_500, 500_000, 500_000));
    assert_eq!(r.bal(&a) - ba, calc::funding_credit(50_000_000, P, rate).unwrap(), "the long pays, rounded up");
    assert_eq!(r.bal(&b) - bb, calc::funding_credit(-50_000_000, P, rate).unwrap(), "the short receives, rounded down");
    assert!(r.bal(&a) - ba < 0 && r.bal(&b) - bb > 0 && (r.bal(&a) - ba) + (r.bal(&b) - bb) <= 0);
    // liquidation price: the long is NOT liquidated one price unit above it and IS at it
    let pos = r.e.accounts[ai as usize].positions[&1].clone();
    let lp = calc::liquidation_price_single(r.bal(&a), pos.size, pos.open_notional, &tiers).expect("a liquidation price");
    assert!(lp < P && lp > P * 88 / 100, "about 9.4% below entry: {lp}");
    for p in [P * 96 / 100, P * 93 / 100, lp + 1] {
        let out = r.block(3000, &[(1, p)], vec![]);
        assert!(!out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })), "not liquidated at {p} (liquidation price {lp})");
        assert_eq!(r.e.markets[&1].mark, p, "empty book: mark = index");
    }
    let out = r.block(3000, &[(1, lp)], vec![]);
    assert!(out.events.iter().any(|e| matches!(e, Event::Liquidated { account } if *account == ai)), "liquidated exactly at {lp}");
    // and the short side, on a fresh rig
    let mut r = Rig::one();
    let (d1, d2) = (r.dep(a, 3_500), r.dep(b, 100_000));
    r.block(3000, &[(1, P)], vec![d1, d2]);
    r.trade(&[(1, P)], 1, &ka, &kb, P, 50_000_000); // a is the maker: short 0.5 BTC
    let ai = r.ai(&a);
    let pos = r.e.accounts[ai as usize].positions[&1].clone();
    assert_eq!(pos.size, -50_000_000);
    let lp = calc::liquidation_price_single(r.bal(&a), pos.size, pos.open_notional, &tiers).unwrap();
    for p in [P * 104 / 100, P * 107 / 100, lp - 1] {
        let out = r.block(3000, &[(1, p)], vec![]);
        assert!(!out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })), "short not liquidated at {p} (liquidation price {lp})");
    }
    let out = r.block(3000, &[(1, lp)], vec![]);
    assert!(out.events.iter().any(|e| matches!(e, Event::Liquidated { account } if *account == ai)), "short liquidated exactly at {lp}");
    let _ = bi;
}
