//! Agent 436 (round 2 review) demonstrations against node round-2 commit `4f127c2` (oracle ingest pool). Copy into
//! `node/tests/` of an export of that commit: `cargo test --release --test r436_node_r2 -- --nocapture`.
//! A test PASSES when the DEFECT IS PRESENT. THROWAWAY keys only. UNAUDITED review tooling, never merged.
mod common;
use common::*;
use k256::ecdsa::SigningKey;
use perps_node::dev::{dev_genesis_real_with, DevOptions};
use perps_node::engine::real::RealEngine;
use perps_node::genesis::Genesis;
use perps_node::oracle_pool::observation_digest;
use perps_node::sequencer::Sequencer;
use perps_node::sig::sign;
use perps_node::types::{OraclePrice, SignedObservation};

const P: u64 = 6_725_000_000_000;

fn observation(g: &Genesis, key: &SigningKey, signer: u8, time_ms: u64, prices: &[(u32, u64)]) -> SignedObservation {
    let mut o = SignedObservation { signer, time_ms, prices: prices.iter().map(|(m, p)| OraclePrice { market_id: *m, price: *p, sources: 5 }).collect(), signature: [0u8; 65] };
    o.signature = sign(key, &observation_digest(g, time_ms / 3000, &o));
    o
}

/// Runs rounds through the ingest pool and the sequencer. Signer 6 either reports like the others (`veto` false)
/// or signs the SAME prices with `timeMs` = the last millisecond of the round. Returns, per round, whether the
/// engine accepted the oracle input.
fn rounds(veto: bool) -> Vec<bool> {
    let (g, keys, checked) = dev_genesis_real_with(&format!("r436-12-{veto}"), 7, 5, 10, 0, &DevOptions::default()).unwrap();
    let dir = tmpdir(&format!("r436-12-{veto}"));
    let mut seq = Sequencer::open(g.clone(), keys.sequencer.clone(), Box::new(RealEngine::new(g.clone(), checked).unwrap()), dir, view()).unwrap();
    let mut t = (now() / 3_600_000) * 3_600_000 + 600_000; // a round boundary
    let mut accepted = Vec::new();
    for _ in 0..5 {
        t += 3000;
        let round = t / 3000;
        let last = seq.engine.last_oracle_round();
        // feeders report 100 ms into the round
        for (i, k) in keys.oracle.iter().enumerate() {
            let tm = if veto && i == 6 { round * 3000 + 2999 } else { t + 100 };
            let o = observation(&g, k, i as u8, tm, &[(1, P)]);
            seq.oracle_pool.offer(&g, round, o, last, t + 100).expect("the pool's pre-filter accepts every one of them");
        }
        // all 7 held: the round is ready; the block is produced 300 ms into the round
        let info = seq.produce(t + 300).unwrap().expect("a block");
        let o = &info.outcome.outcomes[0];
        println!("round {round}: 7 observations in the block, oracle input ok = {} {:?}; engine last accepted round {}", o.ok, o.error, seq.engine.last_oracle_round());
        accepted.push(o.ok);
    }
    accepted
}

/// R436-12 (Medium, liveness; a regression in combination). The engine refuses a WHOLE OracleUpdate when ANY ONE
/// observation in it fails a per-observation rule (engine oracle.rs:177-190: signature, round window, future /
/// age bound). On `main` the sequencer chose which observations to include, so one bad signer could be left out.
/// Round 2's ingest pool includes EVERY observation it holds (no selection, R395-9) and its pre-filter does not
/// apply the engine's time bound, so ONE committee signer (of 7, quorum 5: two liars are supposed to be harmless)
/// vetoes every round by signing `timeMs` at the end of the round: `timeMs > T + oracleMaxFutureMs` for any block
/// built in the first 2 s of the round, the update is ORACLE_TIME, the round is gone from the pool, the index never
/// moves, and every market halts STALE 10 s later. A feeder whose clock runs 1.1 s fast does the same by accident.
#[test]
fn r436_12_one_oracle_signer_vetoes_every_round_through_the_no_selection_pool() {
    let honest = rounds(false);
    let vetoed = rounds(true);
    assert!(honest.iter().all(|x| *x), "control: seven honest observations are accepted every round");
    assert!(vetoed.iter().all(|x| !*x), "one signer with timeMs at the end of the round: no round is ever accepted");
}
