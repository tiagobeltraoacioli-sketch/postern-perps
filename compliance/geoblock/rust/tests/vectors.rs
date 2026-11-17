//! The shared conformance vectors of compliance/geoblock/vectors/, run against the Rust port.

use postern_geoblock::{assertion, attestation, decide, list, sanctions};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}
fn read(p: &str) -> Vec<u8> {
    std::fs::read(root().join(p)).unwrap_or_else(|e| panic!("{p}: {e}"))
}
fn json(p: &str) -> Value {
    serde_json::from_slice(&read(p)).unwrap()
}
fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}
fn load(p: &str) -> list::List {
    let b = read(p);
    list::load_list(&serde_json::from_slice(&b).unwrap(), Some(sha256_hex(&b))).unwrap()
}

#[test]
fn decision_vectors() {
    let v = json("vectors/decision-vectors.json");
    let fixture = load("fixtures/geoblock.fixture.json");
    let real = load("../geoblock.json");
    let s = sanctions::load_sanctions(&json("fixtures/sanctions-fixture.json"), sanctions::DEFAULT_MAX_AGE_MS).unwrap();
    assert_eq!(s.built_ms, v["sanctionsBuiltMs"].as_i64().unwrap());
    let cases = v["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 86);
    let mut n = 0;
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let l = match c["list"].as_str() { Some("fixture") => Some(&fixture), Some("real") => Some(&real), _ => None };
        let sc = if c["sanctions"].is_null() { None } else { Some(&s) };
        let d = decide::decide(l, &c["request"], sc);
        let e = &c["expect"];
        assert_eq!(d.allow, e["allow"].as_bool().unwrap(), "{name}: allow ({:?})", d);
        assert_eq!(d.code, e["code"].as_str().unwrap(), "{name}: code");
        assert_eq!(d.http_status as u64, e["httpStatus"].as_u64().unwrap(), "{name}: http");
        assert_eq!(d.exit_only, e["exitOnly"].as_bool().unwrap(), "{name}: exitOnly");
        if let Some(r) = e.get("reasons") {
            let want: Vec<&str> = r.as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
            assert_eq!(d.reasons, want, "{name}: reasons");
        }
        n += 1;
    }
    assert_eq!(n, 86);
}

fn apply(base: &Value, ops: &Value) -> Value {
    let mut o = base.clone();
    for op in ops.as_array().unwrap() {
        let (path, set) = match (op.get("set"), op.get("delete")) {
            (Some(p), _) => (p, true),
            (None, Some(p)) => (p, false),
            _ => panic!("op"),
        };
        let keys: Vec<&str> = path.as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
        let mut t = &mut o;
        for k in &keys[..keys.len() - 1] {
            t = t.get_mut(*k).unwrap();
        }
        let last = keys[keys.len() - 1];
        if set {
            t.as_object_mut().unwrap().insert(last.to_string(), op["value"].clone());
        } else {
            t.as_object_mut().unwrap().remove(last);
        }
    }
    o
}

#[test]
fn list_vectors() {
    let base = json("fixtures/geoblock.fixture.json");
    let v = json("vectors/list-vectors.json");
    let cases = v["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 36);
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let r = list::load_list(&apply(&base, &c["ops"]), None);
        if c["accept"].as_bool().unwrap() {
            assert!(r.is_ok(), "{name}: {:?}", r.err());
        } else {
            let e = r.expect_err(name);
            assert!(e.0.contains(c["errorContains"].as_str().unwrap()), "{name}: {}", e.0);
        }
    }
}

#[test]
fn real_list_guards() {
    let l = load("../geoblock.json");
    assert_eq!(l.version, "0.1.1");
    assert!(l.countries.values().all(|e| e.effect == list::Effect::Deny));
    assert_eq!(l.exit_actions, vec!["CancelOrder", "CancelAll", "PlaceOrder(reduceOnly=true)", "Withdraw"]);
    assert!(l.restricted_subdivision_countries.contains("UA"));
}

#[test]
fn exhaustive_real_list_opens_nothing() {
    let l = load("../geoblock.json");
    let chars: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars().collect();
    for a in &chars {
        for b in &chars {
            let req = serde_json::json!({"mode": "live", "surface": "api", "kind": "action",
                "action": {"type": "PlaceOrder", "reduceOnly": false}, "geo": {"country": format!("{a}{b}"), "trusted": true},
                "signals": {"tor": false, "vpnProxy": false, "hosting": false},
                "addresses": [{"role": "account", "address": "0x00000000000000000000000000000000000000a1"}], "nowMs": 0,
                "requireAttestation": false, "attestation": null});
            let d = decide::decide(Some(&l), &req, None);
            assert!(!d.allow, "{a}{b}");
        }
    }
}

#[test]
fn geo_assertion_vectors() {
    let v = json("vectors/geo-assertion-vectors.json");
    let secret = v["secretUtf8"].as_str().unwrap().as_bytes().to_vec();
    for c in v["cases"].as_array().unwrap() {
        let r = assertion::verify(c["header"].as_str(), c["mac"].as_str(), &secret, c["nowMs"].as_i64().unwrap(), None);
        let got = match r { Ok(_) => "ok", Err(e) => e };
        assert_eq!(got, c["expect"].as_str().unwrap(), "{}", c["name"]);
    }
    let mut cache = assertion::ReplayCache::new(10);
    let c = &v["cases"][0];
    assert!(assertion::verify(c["header"].as_str(), c["mac"].as_str(), &secret, c["nowMs"].as_i64().unwrap(), Some(&mut cache)).is_ok());
    assert_eq!(assertion::verify(c["header"].as_str(), c["mac"].as_str(), &secret, c["nowMs"].as_i64().unwrap(), Some(&mut cache)), Err("replay"));
    assert_eq!(assertion::mac_hex(&[0u8; 31], "x"), None, "short secret");
}

#[test]
fn attestation_vectors() {
    let v = json("vectors/attestation-vectors.json");
    let vault_s = v["vault"].as_str().unwrap();
    let vault = attestation::parse_vault(vault_s).unwrap();
    assert_eq!(format!("0x{}", attestation::domain_separator(&vault).iter().map(|x| format!("{x:02x}")).collect::<String>()), v["domainSeparator"].as_str().unwrap());
    let mut st = BTreeMap::new();
    st.insert(v["statement"]["id"].as_str().unwrap().to_string(), v["statement"]["hash"].as_str().unwrap().to_string());
    let cases = v["vectors"].as_array().unwrap();
    assert_eq!(cases.len(), 13);
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let m = attestation::parse_message(&c["message"]).unwrap();
        let dg = attestation::digest(&m, &vault);
        assert_eq!(format!("0x{}", dg.iter().map(|x| format!("{x:02x}")).collect::<String>()), c["digest"].as_str().unwrap(), "{name}: digest");
        let r = attestation::verify(c, &st, c["nowMs"].as_u64().unwrap(), Some(vault_s)).unwrap();
        assert_eq!(r.status, c["expect"]["status"].as_str().unwrap(), "{name}: {:?}", r.reason);
        if let Some(s) = c["expect"].get("signer") {
            assert_eq!(r.signer.as_deref(), s.as_str(), "{name}");
        }
    }
    for bad in [None, Some(""), Some("0x0000000000000000000000000000000000000000"), Some("vault")] {
        assert!(attestation::verify(&cases[0], &st, 0, bad).is_err(), "no vault: {bad:?}");
    }
    let notices = json("../legal/notices.json");
    assert_eq!(notices["attestation"]["statementId"], v["statement"]["id"]);
    assert_eq!(notices["attestation"]["statementHash"], v["statement"]["hash"]);
    for kv in v["keccakVectors"].as_array().unwrap() {
        let inp = kv["input"].as_str().unwrap();
        let bytes: Vec<u8> = (2..inp.len()).step_by(2).map(|i| u8::from_str_radix(&inp[i..i + 2], 16).unwrap()).collect();
        let h = postern_geoblock::addr::keccak256(&bytes);
        assert_eq!(format!("0x{}", h.iter().map(|x| format!("{x:02x}")).collect::<String>()), kv["keccak256"].as_str().unwrap());
    }
}

#[test]
fn sanctions_loader() {
    let j = json("fixtures/sanctions-fixture.json");
    let s = sanctions::load_sanctions(&j, sanctions::DEFAULT_MAX_AGE_MS).unwrap();
    assert_eq!(s.len(), 3);
    assert!(s.has("0x983A81CA6FB1E441266D2FBCB7D8E530AC2E05A2"));
    let b = s.built_ms as f64;
    assert!(s.fresh(b) && s.fresh(b + 48.0 * 3600e3) && !s.fresh(b + 48.0 * 3600e3 + 1.0) && !s.fresh(b - 1.0));
    let mut drop = j.clone();
    drop["evm"].as_array_mut().unwrap().pop();
    assert_eq!(sanctions::load_sanctions(&drop, 1).unwrap_err().0, "content hash mismatch");
    let mut rev = j.clone();
    rev["evm"].as_array_mut().unwrap().reverse();
    assert!(sanctions::load_sanctions(&rev, 1).unwrap_err().0.contains("not strictly sorted"));
    let full = json("data/sanctions-evm.json");
    let f = sanctions::load_sanctions(&full, sanctions::DEFAULT_MAX_AGE_MS).unwrap();
    assert_eq!(f.len() as u64, full["counts"]["evmAddresses"].as_u64().unwrap());
}
