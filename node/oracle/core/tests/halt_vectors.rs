//! Shared halt and exit vectors (`vectors/halt.json`) for independent implementations of README sections 5, 6
//! and 6a (the engine, 388, has its own). A scripted sequence of blocks: for each, the block time, the committee
//! prices of the round (or none: withheld), the outcome, the canonical state bytes and the exit price.
//! Committee prices are given directly (signatures are covered by `vectors/eip712.json`), applied through the same
//! `step` path by signing with throwaway keys.
//!
//! Regenerate (only on a deliberate rule change): `ORACLE_WRITE_VECTORS=1 cargo test --test halt_vectors`.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use k256::ecdsa::SigningKey;
use perps_oracle::committee::*;
use perps_oracle::types::*;
use perps_oracle::{eip712, keccak256, sig, ROUND_MS};

const VAULT: [u8; 20] = [0xdb; 20];

fn key(i: u8) -> SigningKey {
    SigningKey::from_bytes(
        &keccak256(format!("postern-perps-oracle-halt-vector-{i}").as_bytes()).into(),
    )
    .unwrap()
}

fn params() -> CommitteeParams {
    CommitteeParams {
        verifying_contract: VAULT,
        signers: (0..5)
            .map(|i| sig::address_of(key(i).verifying_key()))
            .collect(),
        quorum: 3,
        max_future_ms: 1000,
        max_age_ms: 6000,
        markets: vec![MarketParams {
            market_id: 1,
            min_sources: 3,
            agree_bps: 100,
            jump_bps: 500,
            max_staleness_ms: 10_000,
            confirm_rounds: 3,
            exit_haircut_bps: 500,
            exit_haircut_per_min_bps: 100,
            exit_max_haircut_bps: 5000,
            max_halt_ms: 3_600_000,
        }],
    }
}

fn upd(round: u64, prices: &[u64]) -> OracleUpdate {
    let dom = eip712::domain_separator(&VAULT);
    OracleUpdate {
        round,
        observations: prices
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                let o = Observation {
                    round,
                    time_ms: round * ROUND_MS + 100,
                    prices: vec![OraclePrice {
                        market_id: 1,
                        price: p,
                        sources: 5,
                    }],
                };
                SignedObservation {
                    signer: i as u8,
                    time_ms: o.time_ms,
                    prices: o.prices.clone(),
                    signature: sig::sign(&key(i as u8), &eip712::digest(&dom, &o)),
                }
            })
            .collect(),
    }
}

fn build() -> String {
    let prm = params();
    let mut st = OracleState::genesis(&prm).unwrap();
    let r0 = 598_296_000u64;
    // (round offset, committee values or empty = withheld, extra ms after the round start for the block time)
    let script: Vec<(u64, Vec<u64>, u64)> = vec![
        (0, vec![650_000; 5], 400),
        (1, vec![650_100; 5], 400),
        (2, vec![650_200; 5], 400),
        (3, vec![651_000, 651_000, 651_100, 650_900, 700_000], 400),
        (4, vec![640_000, 650_000, 660_000, 670_000, 680_000], 400),
        (5, vec![520_000; 5], 400),
        (6, vec![700_000; 5], 400),
        (7, vec![600_000; 5], 400),
        (8, vec![600_100; 5], 400),
        (9, vec![600_200; 5], 400),
        (20, vec![], 0),
        (30, vec![], 0),
        (250, vec![], 0),
        (1300, vec![], 0),
        (1301, vec![600_000; 5], 400),
        (1302, vec![600_000; 5], 400),
        (1303, vec![600_000; 5], 400),
    ];
    let mut out = String::from("{\"_note\":\"UNAUDITED PROTOTYPE shared vectors of README sections 5, 6, 6a; one market, n=5, q=3.\",\"steps\":[\n");
    for (k, (off, vals, extra)) in script.iter().enumerate() {
        let r = r0 + off;
        let t = r * ROUND_MS + extra;
        let stale = begin_block(&prm, &mut st, t);
        let outcome = if vals.is_empty() {
            "\"withheld\"".to_string()
        } else {
            match apply_update(&prm, &mut st, &upd(r, vals), t) {
                Ok(rep) => format!("\"{:?}\"", rep.outcomes[0].1),
                Err(e) => format!("\"{}\"", e.code()),
            }
        };
        let x = exit_price(&prm, &st, 1, t);
        let exit = match x {
            None => "null".to_string(),
            Some(x) => format!(
                "{{\"source\":\"{:?}\",\"low\":{},\"high\":{},\"reference\":{},\"settle\":{},\"ageMs\":{},\"settleDue\":{}}}",
                x.source, x.low, x.high, x.reference, x.settle, x.age_ms, x.settle_due
            ),
        };
        let mut hex = String::new();
        for b in st.encode() {
            write!(hex, "{b:02x}").unwrap();
        }
        writeln!(
            out,
            "{{\"round\":{r},\"blockTime\":{t},\"committee\":{vals:?},\"haltedStaleAtBlockStart\":{},\"outcome\":{outcome},\"exit\":{exit},\"stateBytes\":\"0x{hex}\"}}{}",
            !stale.is_empty(),
            if k + 1 < script.len() { "," } else { "" }
        )
        .unwrap();
    }
    out.push_str("]}\n");
    out
}

#[test]
fn halt_vectors_match_the_committed_file() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vectors/halt.json");
    let v = build();
    if std::env::var_os("ORACLE_WRITE_VECTORS").is_some() {
        fs::write(&path, &v).unwrap();
    }
    assert_eq!(
        fs::read_to_string(&path).expect("vectors/halt.json"),
        v,
        "halt vectors changed: announce a rule change in the cross-area log"
    );
}
