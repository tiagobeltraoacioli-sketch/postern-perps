//! Shared test vectors (`vectors/eip712.json`): typed data, digest, signature, signer and canonical bytes of an
//! `OracleUpdate`. The engine (388) and the node (389) can check their own implementations against them;
//! `vectors/check-with-cast.sh` re-derives every signature with Foundry's `cast` (an independent EIP-712
//! implementation) and its result is committed in `vectors/CAST-CHECK.txt`.
//!
//! Regenerate (only when the format changes on purpose): `ORACLE_WRITE_VECTORS=1 cargo test --test vectors`.

use std::fs;
use std::path::Path;

use perps_oracle::types::{Observation, OraclePrice, OracleUpdate, SignedObservation};
use perps_oracle::{eip712, encoding, sig};
use perps_oracle_feeder::json;
use perps_oracle_feeder::keyfile::dev_key;
use serde_json::{json, Value};

/// The vault of the vectors' (throwaway) deployment: the mock vault address 393 uses on main (`node/mock/`).
const VAULT_HEX: &str = "db0b1676ac81645ca3e2fbe136b6ad00b2eb908c";

fn vault() -> [u8; 20] {
    hex::decode(VAULT_HEX).unwrap().try_into().unwrap()
}

fn typed(o: &Observation) -> Value {
    json!({
        "types": {
            "EIP712Domain": [
                {"name": "name", "type": "string"}, {"name": "version", "type": "string"}, {"name": "chainId", "type": "uint256"},
                {"name": "verifyingContract", "type": "address"}],
            "OracleObservation": [
                {"name": "round", "type": "uint64"}, {"name": "timeMs", "type": "uint64"}, {"name": "prices", "type": "OraclePrice[]"}],
            "OraclePrice": [
                {"name": "marketId", "type": "uint32"}, {"name": "price", "type": "uint64"}, {"name": "sources", "type": "uint8"}]
        },
        "primaryType": "OracleObservation",
        "domain": {"name": eip712::DOMAIN_NAME, "version": eip712::DOMAIN_VERSION, "chainId": eip712::DOMAIN_CHAIN_ID, "verifyingContract": format!("0x{VAULT_HEX}")},
        "message": {
            "round": o.round, "timeMs": o.time_ms,
            "prices": o.prices.iter().map(|p| json!({"marketId": p.market_id, "price": p.price, "sources": p.sources})).collect::<Vec<_>>()
        }
    })
}

fn build() -> Value {
    let cases: Vec<(&str, Observation)> = vec![
        (
            "one-market",
            Observation {
                round: 598_296_643,
                time_ms: 1_794_889_929_012,
                prices: vec![OraclePrice {
                    market_id: 1,
                    price: 648_703,
                    sources: 7,
                }],
            },
        ),
        (
            "three-markets",
            Observation {
                round: 598_296_644,
                time_ms: 1_794_889_932_000,
                prices: vec![
                    OraclePrice {
                        market_id: 1,
                        price: 648_703,
                        sources: 7,
                    },
                    OraclePrice {
                        market_id: 2,
                        price: 182_784,
                        sources: 6,
                    },
                    OraclePrice {
                        market_id: 3,
                        price: 72_660,
                        sources: 5,
                    },
                ],
            },
        ),
        (
            "max-values",
            Observation {
                round: u64::MAX / 3000 - 1,
                time_ms: (u64::MAX / 3000 - 1) * 3000 + 2999,
                prices: vec![OraclePrice {
                    market_id: u32::MAX,
                    price: u64::MAX,
                    sources: 255,
                }],
            },
        ),
    ];
    let mut out = Vec::new();
    let mut signed = Vec::new();
    for (i, (name, o)) in cases.iter().enumerate() {
        let k = dev_key(&format!("vector-{i}"));
        let d = eip712::digest(&eip712::domain_separator(&vault()), o);
        let s = sig::sign(&k, &d);
        assert_eq!(sig::recover(&d, &s), Ok(sig::address_of(k.verifying_key())));
        out.push(json!({
            "name": name,
            "throwawayScalar": format!("0x{}", hex::encode(k.to_bytes())),
            "signer": format!("0x{}", hex::encode(sig::address_of(k.verifying_key()))),
            "typedData": typed(o),
            "hashStruct": format!("0x{}", hex::encode(eip712::hash_observation(o))),
            "digest": format!("0x{}", hex::encode(d)),
            "signature": format!("0x{}", hex::encode(s)),
        }));
        signed.push(SignedObservation {
            signer: i as u8,
            time_ms: o.time_ms,
            prices: o.prices.clone(),
            signature: s,
        });
    }
    // One OracleUpdate carrying the first case twice under signers 0 and 1 (structure only; signer 1's signature
    // is signer 0's, so the engine rejects it: ORACLE_BAD_SIGNATURE at observation 1).
    let u = OracleUpdate {
        round: cases[0].1.round,
        observations: vec![
            signed[0].clone(),
            SignedObservation {
                signer: 1,
                ..signed[0].clone()
            },
        ],
    };
    json!({
        "_note": "UNAUDITED PROTOTYPE test vectors. Keys are THROWAWAY keys derived from public labels: never use them.",
        "verifyingContract": format!("0x{VAULT_HEX}"),
        "domainSeparator": format!("0x{}", hex::encode(eip712::domain_separator(&vault()))),
        "typeHashes": {
            "OracleObservation": format!("0x{}", hex::encode(perps_oracle::keccak256(eip712::OBSERVATION_TYPE.as_bytes()))),
            "OraclePrice": format!("0x{}", hex::encode(perps_oracle::keccak256(eip712::PRICE_TYPE.as_bytes()))),
        },
        "observations": out,
        "update": {"json": json::update_to_json(&u), "canonicalBytes": format!("0x{}", hex::encode(encoding::encode(&u)))},
    })
}

#[test]
fn vectors_match_the_committed_file() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vectors/eip712.json");
    let v = build();
    if std::env::var_os("ORACLE_WRITE_VECTORS").is_some() {
        fs::write(&path, serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
    }
    let committed: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("vectors/eip712.json")).unwrap();
    assert_eq!(
        committed, v,
        "vectors changed: a format change must be deliberate and announced in the cross-area log"
    );
}
