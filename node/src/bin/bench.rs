//! `perps-bench`: the sequencer path measured in process, on the REAL engine, with a workload that matches.
//!
//! Method (everything in one process, no HTTP; THROWAWAY keys; a fresh data directory on the disk named by
//! `--data`, with both fsyncs per block as in production):
//! 1. genesis from the compliance files (dev overrides as `dev`), `--accounts` accounts funded by deposit inputs,
//!    BTC-USD activated by three oracle rounds signed by the seven throwaway oracle keys through the ingest pool;
//!    a standing book of 200 resting orders;
//! 2. per block of `--block-actions` actions: pairs of (maker limit GTC at the index, taker IOC that fills it), all
//!    pre-signed (signing is the client's cost, reported separately); engine time advances 200 ms per block and
//!    an oracle round enters every 15th block;
//! 3. measured with `Instant`: `admit_signed` per action (signature recovery, engine decoder pre-check, nonce and
//!    authority rules, mempool), `produce` per block (build, engine apply on a copy, state root, both fsyncs,
//!    sequencer signature, market-data index, book deltas, checkpoint body and frame at checkpoint heights),
//!    and, with `--validators 1`, `Validator::process` of the same block by ONE validator (replay, root check,
//!    checkpoint signature); seven run in parallel processes or tasks in a deployment, so one is the latency;
//! 4. then the data directory is reopened twice: from the newest snapshot (`--snapshot-every`) and, with the
//!    snapshots removed, by replay from genesis.
//!
//! Output: one JSON object. UNAUDITED PROTOTYPE; numbers of a shared build host are indicative.

use perps_node::dev::{dev_genesis_real_with, DevOptions};
use perps_node::engine::real::RealEngine;
use perps_node::engine::Engine;
use perps_node::oracle_pool::observation_digest;
use perps_node::sequencer::{RateLimit, Sequencer};
use perps_node::sig::{key_address, sign, throwaway_key};
use perps_node::types::{DepositEvent, OraclePrice, SignedAction, SignedObservation};
use perps_node::util::{keccak, to_hex};
use perps_node::validator::Validator;
use perps_node::watcher::L2View;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const P: u64 = 6_725_000_000_000;
const TICK: u64 = 50_000_000;
const SIZE: u64 = 20_000; // 0.0002 BTC, the market minimum

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p) as usize]
}

fn dist(mut v: Vec<f64>) -> serde_json::Value {
    let mean = if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
    serde_json::json!({"n": v.len(), "mean": mean, "p50": pct(&mut v, 0.5), "p90": pct(&mut v, 0.9), "p99": pct(&mut v, 0.99), "max": pct(&mut v, 1.0)})
}

fn main() {
    let mut a = HashMap::new();
    let v: Vec<String> = std::env::args().skip(1).collect();
    for w in v.chunks(2) {
        if w.len() == 2 {
            a.insert(w[0].trim_start_matches("--").to_string(), w[1].clone());
        }
    }
    let get = |k: &str, d: u64| a.get(k).and_then(|x| x.parse().ok()).unwrap_or(d);
    let (accounts, blocks, per_block, with_val, snap_every, ci) = (get("accounts", 200), get("blocks", 100), get("block-actions", 1000), get("validators", 1), get("snapshot-every", 50), get("checkpoint-interval", 10));
    let dir = std::path::PathBuf::from(a.get("data").cloned().expect("--data DIR (a fresh directory on the disk to measure)"));
    let _ = std::fs::remove_dir_all(&dir);
    let (g, keys, checked) = dev_genesis_real_with("bench", 7, 5, ci, 0, &DevOptions::default()).unwrap();
    let mk = || -> Box<dyn Engine> { Box::new(RealEngine::new(g.clone(), checked.clone()).unwrap()) };
    let view = Arc::new(Mutex::new(L2View::default()));
    let mut seq = Sequencer::open(g.clone(), keys.sequencer.clone(), mk(), dir.clone(), view.clone()).unwrap();
    seq.rate = RateLimit::new(0.0, 0.0);
    seq.snapshot_every = snap_every;
    seq.block_actions = per_block as usize;
    let mut val = if with_val > 0 { Some(Validator::new(g.clone(), keys.validators[0].clone(), mk(), view.clone(), None).unwrap()) } else { None };

    // accounts and their deposits
    let users: Vec<_> = (0..accounts).map(|i| throwaway_key("bench-user", i)).collect();
    {
        // the vault's own head chain (VAULT.md 5.4), so checkpoint bodies are built and frames kept as in a real run
        let mut v = view.lock().unwrap();
        let mut head = perps_node::vaultfmt::deposit_head0(700_770, &g.vault);
        for (i, k) in users.iter().enumerate() {
            let acct = key_address(k);
            head = perps_node::vaultfmt::deposit_head_next(&head, i as u64, &acct, 1_000_000_000_000);
            v.deposits.insert(i as u64, DepositEvent { index: i as u64, account: acct, amount: 1_000_000_000_000, sender: acct, head, l2_block: 1, l2_block_hash: [1; 32], l2_tx_hash: keccak(&[i as u8, 9]), log_index: i as u32 });
        }
    }
    let mut t = (perps_node::node::now_ms() / 3000) * 3000;
    let mut val_ms: Vec<f64> = Vec::new();
    let step = |seq: &mut Sequencer, t: u64, val: &mut Option<Validator>, val_ms: &mut Vec<f64>| -> (f64, usize) {
        let t0 = Instant::now();
        let info = seq.produce(t).expect("produce").expect("a block");
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Some(v) = val {
            let t1 = Instant::now();
            // the validator's clock is the block's own time: lateness is not what is measured
            let r = v.process(&info.block, info.block.header.time);
            val_ms.push(t1.elapsed().as_secs_f64() * 1000.0);
            if let perps_node::validator::VResult::Refused { reason, .. } = r {
                panic!("validator refused: {reason}");
            }
        }
        (ms, info.outcome.outcomes.len())
    };
    let round = |seq: &mut Sequencer, t: u64| {
        let last = seq.engine.last_oracle_round();
        for (i, k) in keys.oracle.iter().enumerate() {
            let mut o = SignedObservation { signer: i as u8, time_ms: t, prices: vec![OraclePrice { market_id: 1, price: P, sources: 5 }], signature: [0u8; 65] };
            o.signature = sign(k, &observation_digest(&g, t / 3000, &o));
            seq.oracle_pool.offer(&g, t / 3000, o, last, t).unwrap();
        }
    };
    for _ in 0..3 {
        t += 3000;
        round(&mut seq, t);
        while seq.engine.next_deposit_index() < accounts || seq.engine.last_oracle_round() < t / 3000 {
            step(&mut seq, t + 5, &mut val, &mut val_ms);
            t += 1;
        }
        t -= t % 3000;
    }
    val_ms.clear();
    let order = |k: &k256::ecdsa::SigningKey, nonce: u64, is_buy: bool, tif: u8, price: u64| -> SignedAction {
        let msg = serde_json::json!({"account": to_hex(&key_address(k)), "nonce": nonce.to_string(), "marketId": 1, "isBuy": is_buy, "orderType": 0, "timeInForce": tif,
            "price": price.to_string(), "size": SIZE.to_string(), "reduceOnly": false, "triggerPrice": "0", "clientOrderId": to_hex(&[0u8; 32])});
        let val = g.types.registry.val_from_json("PlaceOrder", &msg).unwrap();
        let d = perps_node::actions::action_digest(&g, "PlaceOrder", &val).unwrap();
        SignedAction { type_index: g.types.index_of("PlaceOrder").unwrap(), message: val, signature: sign(k, &d) }
    };
    // a standing book: 100 bids below and 100 asks above the index, from the first 20 accounts
    let mut nonce = t;
    t += 200;
    for i in 0..200u64 {
        nonce += 1;
        let k = &users[(i % 20) as usize];
        let (buy, price) = if i % 2 == 0 { (true, P - TICK * (10 + i / 2)) } else { (false, P + TICK * (10 + i / 2)) };
        seq.admit_signed(order(k, nonce, buy, 0, price), t).expect("standing order");
    }
    step(&mut seq, t, &mut val, &mut val_ms);
    val_ms.clear();

    // the measured run
    let (mut sign_s, mut admit_us, mut produce_ms, mut total_actions, mut fills) = (0.0f64, Vec::new(), Vec::new(), 0u64, 0u64);
    let run0 = Instant::now();
    let (mut admit_total_s, mut produce_total_s) = (0.0f64, 0.0f64);
    for b in 0..blocks {
        t += 200;
        if b % 15 == 0 {
            let rt = t - t % 3000;
            if rt / 3000 > seq.engine.last_oracle_round() {
                round(&mut seq, rt.max(t - 900));
            }
        }
        // pre-sign the block's actions: maker rests at the index, taker lifts it
        let ts = Instant::now();
        let mut batch = Vec::with_capacity(per_block as usize);
        for i in 0..per_block / 2 {
            nonce += 1;
            let maker = &users[((b * 7 + i * 2) % accounts) as usize];
            let taker = &users[((b * 7 + i * 2 + 1) % accounts) as usize];
            let maker_sells = (b + i) % 2 == 0;
            batch.push(order(maker, nonce, !maker_sells, 0, P));
            batch.push(order(taker, nonce, maker_sells, 1, P));
        }
        sign_s += ts.elapsed().as_secs_f64();
        let ta = Instant::now();
        for a in batch {
            let t1 = Instant::now();
            seq.admit_signed(a, t).expect("admission");
            admit_us.push(t1.elapsed().as_secs_f64() * 1e6);
        }
        admit_total_s += ta.elapsed().as_secs_f64();
        let (ms, n) = step(&mut seq, t, &mut val, &mut val_ms);
        produce_total_s += ms / 1000.0;
        produce_ms.push(ms);
        total_actions += n as u64;
        fills += seq.recent.back().map(|i| i.delta.as_ref().map(|d| d.trades.len() as u64).unwrap_or(0)).unwrap_or(0);
    }
    let wall = run0.elapsed().as_secs_f64();
    let (height, root) = (seq.log.last_height, seq.engine.state_root());
    let ix = seq.index.as_ref().unwrap();
    let (mismatches, digest) = (ix.mismatches, ix.digest());
    let log_bytes = std::fs::metadata(dir.join("blocks.log")).map(|m| m.len()).unwrap_or(0);
    let snaps = perps_node::log::list_snapshots(&dir);
    let snap_bytes = snaps.iter().map(|s| std::fs::metadata(&s.1).map(|m| m.len()).unwrap_or(0)).max().unwrap_or(0);
    drop(seq);

    // recovery: newest snapshot + tail, then full replay from genesis
    let t0 = Instant::now();
    let s1 = Sequencer::open(g.clone(), keys.sequencer.clone(), mk(), dir.clone(), Arc::new(Mutex::new(L2View::default()))).unwrap();
    let snap_open_s = t0.elapsed().as_secs_f64();
    assert_eq!(s1.engine.state_root(), root);
    let snap_report = format!("{:?}", s1.recovered);
    let snap_digest_ok = s1.index.as_ref().map(|i| i.digest() == digest);
    drop(s1);
    for (_, p) in perps_node::log::list_snapshots(&dir) {
        let _ = std::fs::remove_file(p);
    }
    let t0 = Instant::now();
    let s2 = Sequencer::open(g.clone(), keys.sequencer.clone(), mk(), dir.clone(), Arc::new(Mutex::new(L2View::default()))).unwrap();
    let replay_s = t0.elapsed().as_secs_f64();
    assert_eq!(s2.engine.state_root(), root);
    assert_eq!(s2.index.as_ref().unwrap().digest(), digest, "the index after a replay from genesis equals the live one");
    println!(
        "{}",
        serde_json::json!({
            "accounts": accounts, "blocks": blocks, "actionsPerBlock": per_block, "checkpointInterval": ci, "snapshotEvery": snap_every,
            "inputsApplied": total_actions, "fills": fills, "indexMismatches": mismatches, "height": height,
            "signSecondsClientSide": sign_s,
            "admission": {"totalSeconds": admit_total_s, "perSecond": (blocks * per_block) as f64 / admit_total_s, "microsPerAction": dist(admit_us)},
            "produceMsPerBlock": dist(produce_ms.clone()),
            "produce": {"totalSeconds": produce_total_s, "actionsPerSecond": total_actions as f64 / produce_total_s},
            "sequencerPath": {"admitPlusProduceSeconds": admit_total_s + produce_total_s, "actionsPerSecond": total_actions as f64 / (admit_total_s + produce_total_s)},
            "validatorProcessMsPerBlock": dist(val_ms),
            "wallSecondsIncludingSigning": wall,
            "logBytes": log_bytes, "snapshotBytes": snap_bytes,
            "recovery": {"fromSnapshotSeconds": snap_open_s, "fromSnapshot": snap_report, "indexRestoredEqual": snap_digest_ok,
                "replayFromGenesisSeconds": replay_s, "replayActionsPerSecond": total_actions as f64 / replay_s},
        })
    );
}
