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
