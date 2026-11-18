//! The engine's copy of the oracle rules against agent 394's shared vectors (codex/perps-oracle-20261117 ccdb290):
//! committee rule C3/C4, halt machine H1-H3, exit band X0-X3 (`halt.json`, 17 blocks), and the OracleObservation
//! EIP-712 digests, signatures and canonical OracleUpdate bytes (`eip712.json`).
use postern_perps_engine::codec::{decode_payload, encode_input};
use postern_perps_engine::eip712::{digest, domain_separator, oracle_struct_hash, recover};
use postern_perps_engine::genesis::OracleParams;
use postern_perps_engine::hash::{hex0x, parse_addr};
use postern_perps_engine::oracle::{within_bps, OracleState, ROUND_MS};
use postern_perps_engine::*;
use serde_json::Value;

fn params() -> OracleParams {
    // 394's README section 8 defaults, the parameters of halt.json (n = 5, q = 3)
    OracleParams { min_sources: 3, agree_bps: 100, jump_bps: 500, max_staleness_ms: 10_000, confirm_rounds: 3, exit_haircut_bps: 500, exit_haircut_per_min_bps: 100, exit_max_haircut_bps: 5000, max_halt_ms: 3_600_000 }
}

#[test]
fn halt_vectors_reproduce_394s_exit_band() {
    let v: Value = serde_json::from_str(include_str!("fixtures/oracle-ccdb290/halt.json")).unwrap();
    let p = params();
    let q = 3usize;
    let mut s = OracleState::default();
    let mut checked = 0;
    for st in v["steps"].as_array().unwrap() {
        let t = st["blockTime"].as_u64().unwrap();
        let round = st["round"].as_u64().unwrap();
        let before = s.status;
        s.h1(t, &p);
        let staled = before == postern_perps_engine::oracle::Status::Active && s.status != before;
        assert_eq!(staled, st["haltedStaleAtBlockStart"].as_bool().unwrap(), "H1 at round {round}");
        let mut vals: Vec<u64> = st["committee"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect();
        if vals.len() >= q {
            vals.sort_unstable();
            let m = vals[vals.len() / 2];
            let agree = vals.iter().filter(|x| within_bps(**x, m, p.agree_bps)).count();
            if agree >= q {
                s.consume(m, round * ROUND_MS, &p);
            } else {
                assert!(st["outcome"].as_str().unwrap().starts_with("Disagree"), "round {round}");
            }
        }
        match (&st["exit"], s.exit_price(&p, t)) {
            (Value::Null, None) => {}
            (x, Some(e)) => {
                let got = (e.low, e.high, e.settle, e.settle_due);
                let want = (x["low"].as_u64().unwrap(), x["high"].as_u64().unwrap(), x["settle"].as_u64().unwrap(), x["settleDue"].as_bool().unwrap());
                assert_eq!(got, want, "exit band at round {round} ({})", st["outcome"]);
                checked += 1;
            }
            (x, None) => panic!("round {round}: engine has no exit price, 394 says {x}"),
        }
    }
    assert_eq!(checked, 15, "15 of the 17 blocks carry an exit price (two are before the first index)");
}

#[test]
fn eip712_and_canonical_bytes_match_394() {
    let v: Value = serde_json::from_str(include_str!("fixtures/oracle-ccdb290/eip712.json")).unwrap();
    let vault = parse_addr(v["verifyingContract"].as_str().unwrap()).unwrap();
    let ds = domain_separator(700770, &vault);
    assert_eq!(hex0x(&ds), v["domainSeparator"].as_str().unwrap());
    let mut n = 0;
    for o in v["observations"].as_array().unwrap() {
        let m = &o["typedData"]["message"];
        let prices: Vec<OraclePrice> = m["prices"].as_array().unwrap().iter().map(|x| OraclePrice {
            market_id: x["marketId"].as_u64().unwrap() as u32,
            price: x["price"].as_u64().unwrap(),
            sources: x["sources"].as_u64().unwrap() as u8,
        }).collect();
        let sh = oracle_struct_hash(m["round"].as_u64().unwrap(), m["timeMs"].as_u64().unwrap(), &prices);
        assert_eq!(hex0x(&sh), o["hashStruct"].as_str().unwrap(), "{}", o["name"]);
        let d = digest(&ds, &sh);
        assert_eq!(hex0x(&d), o["digest"].as_str().unwrap(), "{}", o["name"]);
        let sig: [u8; 65] = hex::decode(&o["signature"].as_str().unwrap()[2..]).unwrap().try_into().unwrap();
        assert_eq!(recover(&d, &sig).map(|a| hex0x(&a)), Some(o["signer"].as_str().unwrap().to_lowercase()), "{}", o["name"]);
        n += 1;
    }
    assert_eq!(n, 3);
    let bytes = hex::decode(&v["update"]["canonicalBytes"].as_str().unwrap()[2..]).unwrap();
    let input = decode_payload(0x01, &bytes).expect("394's canonical OracleUpdate decodes strictly");
    let enc = encode_input(&input);
    assert_eq!(&enc[5..], &bytes[..], "and re-encodes byte-identically");
}
