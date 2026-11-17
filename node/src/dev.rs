//! Dev-mode genesis: THROWAWAY keys derived from a seed, the fixture typed data and markets, the owner's fee
//! address (a public fact: `0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544`, BRIEF-PERPS), a throwaway vault address.
//! Nothing here is a real key.
//!
//! UNAUDITED PROTOTYPE.

use crate::genesis::Genesis;
use crate::sig::{key_address, throwaway_key};
use crate::util::{keccak, parse_address, Address};
use k256::ecdsa::SigningKey;
use std::sync::Arc;

pub const OWNER_FEE_ADDRESS: &str = "0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544";

pub struct DevKeys {
    pub sequencer: SigningKey,
    /// committee keys sorted by address ascending
    pub validators: Vec<SigningKey>,
    pub oracle: Vec<SigningKey>,
}

pub fn sorted_keys(label: &str, n: usize) -> Vec<SigningKey> {
    let mut v: Vec<SigningKey> = (0..n as u64).map(|i| throwaway_key(label, i)).collect();
    v.sort_by_key(key_address);
    v
}

pub fn dev_keys(seed: &str, n: usize) -> DevKeys {
    DevKeys {
        sequencer: throwaway_key(&format!("{seed}/sequencer"), 0),
        validators: sorted_keys(&format!("{seed}/validator"), n),
        oracle: sorted_keys(&format!("{seed}/oracle"), n),
    }
}

pub fn dev_vault(seed: &str) -> Address {
    keccak(format!("postern-perps THROWAWAY dev vault | {seed}").as_bytes())[12..].try_into().unwrap()
}

pub fn fixture(name: &str) -> Vec<u8> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("fixture {}: {e}", p.display()))
}

pub fn dev_genesis(seed: &str, n: usize, threshold: usize, checkpoint_interval: u64, genesis_time: u64) -> (Arc<Genesis>, DevKeys) {
    let k = dev_keys(seed, n);
    let g = Genesis::new(
        &fixture("eip712.json"),
        &fixture("markets.json"),
        k.validators.iter().map(key_address).collect(),
        threshold,
        k.oracle.iter().map(key_address).collect(),
        threshold,
        key_address(&k.sequencer),
        dev_vault(seed),
        parse_address(OWNER_FEE_ADDRESS).unwrap(),
        genesis_time,
        checkpoint_interval,
    )
    .expect("dev genesis");
    (Arc::new(g), k)
}

pub fn compliance_file(name: &str) -> Vec<u8> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../compliance").join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("compliance file {}: {e}", p.display()))
}

/// Dev genesis for the REAL engine (agent 388's): markets from 392's `compliance/markets.json` + `risk.json` through
/// the engine's converter; the node's genesis reads the SAME markets file (ids, symbols, display decimals), its
/// withdrawal fee is the engine's, and `logHash(0)` is the engine's. THROWAWAY keys only.
pub fn dev_genesis_real(seed: &str, n: usize, threshold: usize, checkpoint_interval: u64, genesis_time: u64) -> (Arc<Genesis>, DevKeys, postern_perps_engine::genesis::Checked) {
    let k = dev_keys(seed, n);
    let mb = compliance_file("markets.json");
    let rb = compliance_file("risk.json");
    let mut g = Genesis::new(
        &fixture("eip712.json"),
        &mb,
        k.validators.iter().map(key_address).collect(),
        threshold,
        k.oracle.iter().map(key_address).collect(),
        threshold,
        key_address(&k.sequencer),
        dev_vault(seed),
        parse_address(OWNER_FEE_ADDRESS).unwrap(),
        genesis_time,
        checkpoint_interval,
    )
    .expect("dev genesis (compliance markets)");
    let mj: serde_json::Value = serde_json::from_slice(&mb).unwrap();
    let rj: serde_json::Value = serde_json::from_slice(&rb).unwrap();
    let checked = crate::engine::real::dev_engine_genesis_compliance(&g, &mj, &rj).expect("dev engine genesis");
    g.withdrawal_fee = checked.g.withdrawal_fee as u128;
    let probe = crate::engine::real::RealEngine::new(Arc::new(g.clone()), checked.clone()).expect("engine and node genesis agree");
    use crate::engine::Engine;
    let g2 = g.with_log_hash0(probe.genesis_log_hash());
    (Arc::new(g2), k, checked)
}

/// Dev prices for the dev oracle (USD per base unit, on each compliance market's tick).
pub fn dev_prices() -> Vec<(u32, u64)> {
    vec![(1, 6_725_000_000_000), (2, 348_000_000_000), (3, 18_240_000_000)]
}
