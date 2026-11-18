//! Agent 436: fixed-property ports of my node demonstrations, for node `90d8787` (431's fixes of R436-3, -4, -12).
//! Same scenarios as `r436_node.rs` / `r436_node_r2.rs` (which passed while the defects were present and FAIL on
//! `90d8787`), with the assertions turned into the fixed property. Copy into `node/tests/`. THROWAWAY keys only.
mod common;
use common::*;
use k256::ecdsa::SigningKey;
use perps_node::dev::{dev_genesis_real_with, DevOptions};
use perps_node::engine::real::RealEngine;
use perps_node::genesis::Genesis;
use perps_node::oracle_pool::observation_digest;
use perps_node::sequencer::Sequencer;
use perps_node::sig::{key_address, sign};
use perps_node::types::{OraclePrice, SignedObservation};

const P: u64 = 6_725_000_000_000;

fn observation(g: &Genesis, key: &SigningKey, signer: u8, time_ms: u64, prices: &[(u32, u64)]) -> SignedObservation {
    let mut o = SignedObservation { signer, time_ms, prices: prices.iter().map(|(m, p)| OraclePrice { market_id: *m, price: *p, sources: 5 }).collect(), signature: [0u8; 65] };
    o.signature = sign(key, &observation_digest(g, time_ms / 3000, &o));
    o
}

/// R436-12 fixed: signer 6 signs every round at its last millisecond. No round is lost: the block built 300 ms into
/// the round carries no oracle input (the round waits), the block built 2,100 ms into it carries all seven
/// observations and the engine accepts them. Five rounds, five accepted.
#[test]
fn r436_12_fixed_a_late_signer_delays_each_round_and_vetoes_none() {
    let (g, keys, checked) = dev_genesis_real_with("r436-12-fixed", 7, 5, 10, 0, &DevOptions::default()).unwrap();
    let dir = tmpdir("r436-12-fixed");
    let mut seq = Sequencer::open(g.clone(), keys.sequencer.clone(), Box::new(RealEngine::new(g.clone(), checked).unwrap()), dir, view()).unwrap();
    let mut t = (now() / 3_600_000) * 3_600_000 + 600_000;
    let mut accepted = 0;
    for _ in 0..5 {
        t += 3000;
        let round = t / 3000;
        let last = seq.engine.last_oracle_round();
        for (i, k) in keys.oracle.iter().enumerate() {
            let tm = if i == 6 { round * 3000 + 2999 } else { t + 100 };
            seq.oracle_pool.offer(&g, round, observation(&g, k, i as u8, tm, &[(1, P)]), last, t + 100).unwrap();
        }
        let early = seq.produce(t + 300).unwrap();
        assert!(early.is_none(), "300 ms into the round: nothing to include yet (the round waits for the late observation)");
        let info = seq.produce(t + 2100).unwrap().expect("a block 2,100 ms into the round");
        let o = &info.outcome.outcomes[0];
        println!("round {round}: block at +2100 ms, oracle input ok = {} {:?}, engine last accepted round {}", o.ok, o.error, seq.engine.last_oracle_round());
        assert!(o.ok && seq.engine.last_oracle_round() == round);
        accepted += 1;
    }
    assert_eq!(accepted, 5);
}

/// R436-3 fixed: 10,001 finalized deposits are credited over several blocks, in vault order, and the sequencer
/// keeps running. R436-4 fixed: the 33-byte-name ApproveAgent is refused at admission and an honest action sent
/// with it is included.
#[test]
fn r436_3_and_4_fixed_deposits_spread_over_blocks_and_the_poison_action_is_refused() {
    let mut n = net_real("r436-34-fixed", 10);
    let g = n.g.clone();
    {
        let mut v = n.seq_view.lock().unwrap();
        for i in 0..10_001u64 {
            v.deposits.insert(i, deposit(i, key_address(&user(1_000 + (i % 50))), 10_000_000));
        }
    }
    let mut blocks = 0;
    while n.seq.engine.next_deposit_index() < 10_001 {
        n.seq.produce(now()).expect("no halt").expect("a block");
        blocks += 1;
        assert!(blocks < 50);
    }
    println!("10,001 deposits credited in {blocks} blocks; halted = {:?}", n.seq.halted);
    assert!(n.seq.halted.is_none() && blocks >= 6, "at most 2,000 deposits per block");
    let honest = user(1_000);
    let ha = key_address(&honest);
    let t = now();
    let k = user(7);
    let poison = signed(
        &g,
        &k,
        "ApproveAgent",
        serde_json::json!({"account": perps_node::util::to_hex(&key_address(&k)), "nonce": (t + 2).to_string(), "agent": perps_node::util::to_hex(&[9u8; 20]),
            "name": "x".repeat(33), "expiresAt": (t + 3_600_000).to_string()}),
    );
    let r = n.seq.admit(&request(&g, &poison), t);
    println!("33-byte name: {:?}", r.as_ref().map(|a| a.status).map_err(|e| (e.http, e.code)));
    assert_eq!(r.err().map(|e| (e.http, e.code)), Some((400, "MALFORMED")));
    n.seq.admit_signed(withdraw(&g, &honest, ha, t + 1, 2_000_000, ha), t).expect("the honest action is admitted");
    let info = n.seq.produce(t + 5).unwrap().expect("a block");
    assert!(info.outcome.outcomes.iter().all(|o| o.ok), "the honest withdrawal is included and succeeds");
    assert!(n.seq.halted.is_none());
}
