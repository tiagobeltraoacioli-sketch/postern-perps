//! EIP-712 digest of `OracleObservation` (README section 3).
//!
//! Domain `EIP712Domain(string name,string version,uint256 chainId)` = `{PosternPerps, 1, 7007701}`: the engine's
//! signing domain of BRIEF-PERPS, without `verifyingContract` (the engine is not a contract). The primary type
//! separates an oracle observation from every user action of the same domain. Digests are cross-checked against an
//! independent implementation (Foundry's `cast wallet sign --data`) in `vectors/eip712.json`.

use crate::keccak256;
use crate::types::{Observation, OraclePrice};

pub const DOMAIN_NAME: &str = "PosternPerps";
pub const DOMAIN_VERSION: &str = "1";
pub const DOMAIN_CHAIN_ID: u64 = 7_007_701;

pub const DOMAIN_TYPE: &str = "EIP712Domain(string name,string version,uint256 chainId)";
pub const PRICE_TYPE: &str = "OraclePrice(uint32 marketId,uint64 price,uint8 sources)";
/// The primary type followed by its referenced types in alphabetical order (EIP-712 `encodeType`).
pub const OBSERVATION_TYPE: &str =
    "OracleObservation(uint64 round,uint64 timeMs,OraclePrice[] prices)OraclePrice(uint32 marketId,uint64 price,uint8 sources)";

fn word_u64(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

pub fn domain_separator() -> [u8; 32] {
    domain_separator_for(DOMAIN_NAME, DOMAIN_VERSION, DOMAIN_CHAIN_ID)
}

pub fn domain_separator_for(name: &str, version: &str, chain_id: u64) -> [u8; 32] {
    let mut b = Vec::with_capacity(32 * 4);
    b.extend_from_slice(&keccak256(DOMAIN_TYPE.as_bytes()));
    b.extend_from_slice(&keccak256(name.as_bytes()));
    b.extend_from_slice(&keccak256(version.as_bytes()));
    b.extend_from_slice(&word_u64(chain_id));
    keccak256(&b)
}

pub fn hash_price(p: &OraclePrice) -> [u8; 32] {
    let mut b = Vec::with_capacity(32 * 4);
    b.extend_from_slice(&keccak256(PRICE_TYPE.as_bytes()));
    b.extend_from_slice(&word_u64(u64::from(p.market_id)));
    b.extend_from_slice(&word_u64(p.price));
    b.extend_from_slice(&word_u64(u64::from(p.sources)));
    keccak256(&b)
}

pub fn hash_observation(o: &Observation) -> [u8; 32] {
    let mut arr = Vec::with_capacity(32 * o.prices.len());
    for p in &o.prices {
        arr.extend_from_slice(&hash_price(p));
    }
    let mut b = Vec::with_capacity(32 * 4);
    b.extend_from_slice(&keccak256(OBSERVATION_TYPE.as_bytes()));
    b.extend_from_slice(&word_u64(o.round));
    b.extend_from_slice(&word_u64(o.time_ms));
    b.extend_from_slice(&keccak256(&arr));
    keccak256(&b)
}

/// `keccak256(0x19 0x01 || domainSeparator || hashStruct(observation))`.
pub fn digest(o: &Observation) -> [u8; 32] {
    digest_with_domain(&domain_separator(), o)
}

pub fn digest_with_domain(domain: &[u8; 32], o: &Observation) -> [u8; 32] {
    let mut b = Vec::with_capacity(66);
    b.extend_from_slice(&[0x19, 0x01]);
    b.extend_from_slice(domain);
    b.extend_from_slice(&hash_observation(o));
    keccak256(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_strings_are_canonical() {
        // encodeType: no spaces after commas, referenced types appended in alphabetical order.
        assert!(OBSERVATION_TYPE.ends_with(PRICE_TYPE));
        assert!(!OBSERVATION_TYPE.contains(", "));
    }

    #[test]
    fn every_field_moves_the_digest() {
        let base = Observation {
            round: 5,
            time_ms: 15_001,
            prices: vec![OraclePrice { market_id: 1, price: 100, sources: 3 }],
        };
        let d = digest(&base);
        let mut v = base.clone();
        v.round = 6;
        assert_ne!(digest(&v), d);
        let mut v = base.clone();
        v.time_ms += 1;
        assert_ne!(digest(&v), d);
        let mut v = base.clone();
        v.prices[0].market_id = 2;
        assert_ne!(digest(&v), d);
        let mut v = base.clone();
        v.prices[0].price = 101;
        assert_ne!(digest(&v), d);
        let mut v = base.clone();
        v.prices[0].sources = 4;
        assert_ne!(digest(&v), d);
        let mut v = base.clone();
        v.prices.push(OraclePrice { market_id: 2, price: 1, sources: 3 });
        assert_ne!(digest(&v), d);
        assert_ne!(digest_with_domain(&domain_separator_for(DOMAIN_NAME, DOMAIN_VERSION, 700_770), &base), d);
        assert_ne!(digest_with_domain(&domain_separator_for("PosternPerpsVault", DOMAIN_VERSION, DOMAIN_CHAIN_ID), &base), d);
    }
}
